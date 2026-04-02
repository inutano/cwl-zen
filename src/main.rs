use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use serde_json;

use cwl_zen::container;
use cwl_zen::dag;
use cwl_zen::execute;
use cwl_zen::input;
use cwl_zen::model::{CwlDocument, RuntimeContext};
use cwl_zen::parse;
use cwl_zen::staging::StagingMode;

#[derive(Parser)]
#[command(name = "cwl-zen", version, about = "A minimal, JS-free CWL v1.2 runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a CWL workflow or tool
    Run {
        /// Path to the CWL file
        cwl_file: PathBuf,

        /// Path to the input YAML file
        input_file: PathBuf,

        /// Output directory (default: ./cwl-zen-output)
        #[arg(long, default_value = "./cwl-zen-output")]
        outdir: PathBuf,

        /// Skip RO-Crate provenance generation
        #[arg(long)]
        no_crate: bool,

        /// Container engine override (auto-detected if not set)
        #[arg(long, value_parser = ["docker", "podman", "singularity", "apptainer"])]
        engine: Option<String>,

        /// Container image cache directory
        #[arg(long)]
        container_cache: Option<PathBuf>,

        /// Force copy-staging for all inputs (instead of symlinks)
        #[arg(long)]
        copy_inputs: bool,

        /// Disable automatic retry with copy-staging on symlink failures
        #[arg(long)]
        no_retry_copy: bool,
    },

    /// Validate one or more CWL files
    Validate {
        /// CWL files to validate
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },

    /// Print the execution DAG for a workflow
    Dag {
        /// Path to the CWL workflow file
        cwl_file: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            cwl_file,
            input_file,
            outdir,
            no_crate,
            engine,
            container_cache,
            copy_inputs,
            no_retry_copy,
        } => cmd_run(
            &cwl_file,
            &input_file,
            &outdir,
            no_crate,
            engine.as_deref(),
            container_cache.as_deref(),
            copy_inputs,
            no_retry_copy,
        ),

        Commands::Validate { files } => cmd_validate(&files),

        Commands::Dag { cwl_file } => cmd_dag(&cwl_file),
    }
}

fn cmd_run(
    cwl_file: &Path,
    input_file: &Path,
    outdir: &Path,
    no_crate: bool,
    engine_name: Option<&str>,
    container_cache: Option<&Path>,
    copy_inputs: bool,
    no_retry_copy: bool,
) {
    // 1. Parse CWL file
    let doc = match parse::parse_cwl(cwl_file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing CWL file: {e:#}");
            process::exit(1);
        }
    };

    // 2. Parse input YAML (base_dir = input_file's parent)
    let base_dir = input_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let inputs = match input::parse_inputs(input_file, &base_dir) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Error parsing input file: {e:#}");
            process::exit(1);
        }
    };

    // 3. Create outdir and canonicalize to absolute path (fixes Docker bind mounts with relative paths)
    if let Err(e) = std::fs::create_dir_all(outdir) {
        eprintln!("Error creating output directory: {e}");
        process::exit(1);
    }
    let outdir = match std::fs::canonicalize(outdir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error resolving output directory: {e}");
            process::exit(1);
        }
    };
    let outdir = outdir.as_path();

    // 4. Resolve container cache directory
    let _cache_dir = container::resolve_container_cache(container_cache);

    // 5. Select container engine
    let engine: Box<dyn container::ContainerEngine> = if let Some(name) = engine_name {
        match container::engine_by_name(name) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Error: {e:#}");
                process::exit(1);
            }
        }
    } else {
        match container::detect_engine() {
            Ok(e) => e,
            Err(_) => {
                eprintln!("Warning: no container engine found; tools without DockerRequirement will still work");
                Box::new(container::OciEngine::docker())
            }
        }
    };
    eprintln!("Container engine: {}", engine.name());

    // 6. Determine staging mode
    let staging_mode = if copy_inputs {
        StagingMode::Copy
    } else {
        StagingMode::Symlink
    };

    match doc {
        CwlDocument::Workflow(wf) => {
            // 5a. Build DAG
            let dag_steps = match dag::build_dag(&wf) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Error building DAG: {e:#}");
                    process::exit(1);
                }
            };

            // Print step names to stderr
            eprintln!("Workflow steps:");
            for step in &dag_steps {
                eprintln!("  - {}", step.name);
            }

            // Execute workflow
            let result = match execute::execute_workflow(
                cwl_file,
                &wf,
                &dag_steps,
                &inputs,
                outdir,
                engine.as_ref(),
                staging_mode,
                no_retry_copy,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error executing workflow: {e:#}");
                    process::exit(1);
                }
            };

            if !result.success {
                eprintln!("Workflow execution failed");
                process::exit(1);
            }

            if !no_crate {
                if let Err(e) = cwl_zen::provenance::generate_crate(&result, outdir) {
                    eprintln!("Warning: failed to generate RO-Crate provenance: {e:#}");
                } else {
                    eprintln!("Provenance: {}/ro-crate-metadata.json", outdir.display());
                }
            }

            // Print CWL-style JSON output to stdout (for conformance tests)
            let json_out = cwl_zen::param::outputs_to_json(&result.outputs);
            println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());

            eprintln!("Workflow completed successfully");
            eprintln!("Outputs in: {}", outdir.display());
        }

        CwlDocument::CommandLineTool(tool) => {
            let log_dir = outdir.join("logs");
            let runtime = RuntimeContext {
                cores: 1,
                ram: 1024,
                outdir: outdir.to_string_lossy().to_string(),
                tmpdir: outdir.join("tmp").to_string_lossy().to_string(),
            };

            let (exit_code, outputs) = match execute::execute_tool(
                &tool,
                &inputs,
                outdir,
                &runtime,
                &log_dir,
                "tool",
                engine.as_ref(),
                staging_mode,
                no_retry_copy,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error executing tool: {e:#}");
                    process::exit(1);
                }
            };

            // Print CWL-style JSON output to stdout (for conformance tests)
            let json_out = cwl_zen::param::outputs_to_json(&outputs);
            println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());

            if exit_code != 0 {
                eprintln!("Tool exited with code {exit_code}");
                process::exit(exit_code);
            }
        }
    }
}

fn cmd_validate(files: &[PathBuf]) {
    let mut any_failed = false;

    for file in files {
        match parse::parse_cwl(file) {
            Ok(doc) => {
                let class = match doc {
                    CwlDocument::CommandLineTool(_) => "CommandLineTool",
                    CwlDocument::Workflow(_) => "Workflow",
                };
                println!("PASS  {}  ({})", file.display(), class);
            }
            Err(e) => {
                println!("FAIL  {}  ({e:#})", file.display());
                any_failed = true;
            }
        }
    }

    if any_failed {
        process::exit(1);
    }
}

fn cmd_dag(cwl_file: &Path) {
    let doc = match parse::parse_cwl(cwl_file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing CWL file: {e:#}");
            process::exit(1);
        }
    };

    match doc {
        CwlDocument::Workflow(wf) => {
            let dag_steps = match dag::build_dag(&wf) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Error building DAG: {e:#}");
                    process::exit(1);
                }
            };
            dag::print_dag(&dag_steps);
        }
        CwlDocument::CommandLineTool(_) => {
            eprintln!("Error: {} is a CommandLineTool, not a Workflow", cwl_file.display());
            eprintln!("The 'dag' subcommand requires a Workflow document");
            process::exit(1);
        }
    }
}
