use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::command;
use crate::container::{self, ContainerEngine, ContainerExecRequest};
use crate::dag::DagStep;
use crate::model::{
    CwlDocument, FileValue, ResolvedValue, RuntimeContext, StepInput, Workflow,
};
use crate::param;
use crate::parse;
use crate::stage;
use crate::staging::{self, StagingMode};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a complete workflow run.
#[derive(Debug)]
pub struct RunResult {
    pub workflow_path: PathBuf,
    pub workflow: Workflow,
    pub inputs: HashMap<String, ResolvedValue>,
    pub outputs: HashMap<String, ResolvedValue>,
    pub steps: Vec<StepResult>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub success: bool,
}

/// Result of a single step execution.
#[derive(Debug)]
pub struct StepResult {
    pub step_name: String,
    pub tool_path: PathBuf,
    pub container_image: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub exit_code: i32,
    pub inputs: HashMap<String, ResolvedValue>,
    pub outputs: HashMap<String, ResolvedValue>,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Execute a single CommandLineTool step.
///
/// 1. Create workdir and log_dir
/// 2. Stage inputs into workdir
/// 3. Build command via `command::build_command()` using staged inputs
/// 4. Set up stdout/stderr log files
/// 5. Run via ContainerEngine (if docker_image set) or directly
/// 6. Capture exit code
/// 7. Auto-retry with copy staging if symlink failure detected
/// 8. Handle stdout redirect
/// 9. Collect outputs via `stage::collect_outputs()`
/// 10. Return (exit_code, outputs)
pub fn execute_tool(
    tool: &crate::model::CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    runtime: &RuntimeContext,
    log_dir: &Path,
    step_name: &str,
    engine: &dyn ContainerEngine,
    staging_mode: StagingMode,
    no_retry_copy: bool,
) -> Result<(i32, HashMap<String, ResolvedValue>)> {
    // 1. Create workdir and log_dir
    fs::create_dir_all(workdir)
        .with_context(|| format!("creating workdir: {}", workdir.display()))?;
    fs::create_dir_all(log_dir)
        .with_context(|| format!("creating log_dir: {}", log_dir.display()))?;

    // 2. Stage inputs into workdir
    let staged_inputs = staging::stage_inputs(inputs, workdir, staging_mode)?;

    // 3. Build command using staged inputs
    let resolved_cmd = command::build_command(tool, &staged_inputs, runtime);

    // 4. Set up log files
    let stdout_log = log_dir.join(format!("{}.stdout.log", step_name));
    let stderr_log = log_dir.join(format!("{}.stderr.log", step_name));

    // 5 & 6. Execute the command
    let exit_code = if let Some(ref image) = resolved_cmd.docker_image {
        // Pull image
        let cache_dir = container::resolve_container_cache(None);
        engine.pull(image, &cache_dir)?;

        // Build mounts
        let mounts = container::build_mounts(&staged_inputs, workdir);

        // Execute via engine
        let stdout_file = fs::File::create(&stdout_log)
            .with_context(|| format!("creating stdout log: {}", stdout_log.display()))?;
        let stderr_file = fs::File::create(&stderr_log)
            .with_context(|| format!("creating stderr log: {}", stderr_log.display()))?;

        let req = ContainerExecRequest {
            image: image.clone(),
            command: resolved_cmd.command_line.clone(),
            use_shell: resolved_cmd.use_shell,
            workdir: workdir.to_path_buf(),
            mounts,
            network: resolved_cmd.network_access,
            cores: resolved_cmd.cores,
            ram: resolved_cmd.ram,
            stdout: stdout_file,
            stderr: stderr_file,
        };
        let status = engine.exec(req)?;
        status.code().unwrap_or(1)
    } else {
        // Direct execution (no container)
        let mut cmd = if resolved_cmd.use_shell {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&resolved_cmd.command_line);
            c
        } else {
            let parts: Vec<&str> = resolved_cmd.command_line.split_whitespace().collect();
            if parts.is_empty() {
                bail!("empty command line for step '{}'", step_name);
            }
            let mut c = Command::new(parts[0]);
            for part in &parts[1..] {
                c.arg(part);
            }
            c
        };

        let stdout_file = fs::File::create(&stdout_log)
            .with_context(|| format!("creating stdout log: {}", stdout_log.display()))?;
        let stderr_file = fs::File::create(&stderr_log)
            .with_context(|| format!("creating stderr log: {}", stderr_log.display()))?;

        cmd.stdout(stdout_file).stderr(stderr_file);
        cmd.current_dir(workdir);

        let status = cmd
            .status()
            .with_context(|| format!("running command for step '{}'", step_name))?;
        status.code().unwrap_or(1)
    };

    // 7. Auto-retry with copy staging if symlink failure detected
    if exit_code != 0 && !no_retry_copy && staging_mode == StagingMode::Symlink {
        let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
        if staging::is_symlink_error(&stderr_content) {
            eprintln!(
                "Step '{}' failed with possible symlink error. Retrying with copy-staged inputs...",
                step_name
            );

            // Create a new workdir for the retry
            let retry_workdir = workdir
                .parent()
                .unwrap_or(workdir)
                .join(format!("{}_copy_retry", step_name));
            fs::create_dir_all(&retry_workdir)?;

            // Re-run with Copy mode and no_retry_copy=true to avoid infinite recursion
            let (retry_exit_code, retry_outputs) = execute_tool(
                tool,
                inputs,
                &retry_workdir,
                runtime,
                log_dir,
                &format!("{}_retry", step_name),
                engine,
                StagingMode::Copy,
                true,
            )?;

            if retry_exit_code == 0 {
                eprintln!("Step '{}' succeeded after copy-staging.", step_name);
            }

            return Ok((retry_exit_code, retry_outputs));
        }
    }

    // 8. If stdout redirect: copy stdout log to workdir/{stdout_file}
    if let Some(ref stdout_filename) = resolved_cmd.stdout_file {
        let dest = workdir.join(stdout_filename);
        fs::copy(&stdout_log, &dest).with_context(|| {
            format!(
                "copying stdout log to {}: {}",
                dest.display(),
                stdout_log.display()
            )
        })?;
    }

    // 9. Collect outputs
    let outputs = stage::collect_outputs(tool, &staged_inputs, runtime, workdir)?;

    // 10. Return
    Ok((exit_code, outputs))
}

/// Execute a full workflow: resolve DAG, run steps in order, collect outputs.
pub fn execute_workflow(
    workflow_path: &Path,
    workflow: &Workflow,
    dag: &[DagStep],
    inputs: &HashMap<String, ResolvedValue>,
    outdir: &Path,
    engine: &dyn ContainerEngine,
    staging_mode: StagingMode,
    no_retry_copy: bool,
) -> Result<RunResult> {
    let start_time = Utc::now();

    // Create output and log directories
    fs::create_dir_all(outdir)
        .with_context(|| format!("creating outdir: {}", outdir.display()))?;
    let log_dir = outdir.join("logs");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log dir: {}", log_dir.display()))?;

    // Merge workflow defaults: if an input is not provided, use the workflow
    // input's default value (if declared).
    let mut inputs = inputs.clone();
    for (name, wf_input) in &workflow.inputs {
        if !inputs.contains_key(name) {
            if let Some(default) = &wf_input.default {
                inputs.insert(name.clone(), yaml_to_resolved(default));
            }
        }
    }

    // Resolve the workflow's parent directory for relative tool paths
    let wf_dir = workflow_path
        .parent()
        .unwrap_or_else(|| Path::new("."));

    // Store step outputs for downstream resolution
    let mut step_outputs: HashMap<String, HashMap<String, ResolvedValue>> = HashMap::new();
    let mut step_results: Vec<StepResult> = Vec::new();
    let mut success = true;

    for dag_step in dag {
        let step_start = Utc::now();
        let step_name = &dag_step.name;

        // a. Parse the tool CWL file (relative to workflow directory)
        let tool_path = wf_dir.join(&dag_step.tool_path);
        let doc = parse::parse_cwl(&tool_path)
            .with_context(|| format!("parsing tool for step '{}': {}", step_name, tool_path.display()))?;
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => bail!("step '{}' must reference a CommandLineTool, not a Workflow", step_name),
        };

        // b. Resolve step inputs
        let wf_step = workflow
            .steps
            .get(step_name)
            .with_context(|| format!("step '{}' not found in workflow", step_name))?;
        let step_inputs = resolve_step_inputs(
            &wf_step.inputs,
            &inputs,
            &step_outputs,
            &RuntimeContext {
                cores: 1,
                ram: 1024,
                outdir: outdir.to_string_lossy().to_string(),
                tmpdir: outdir.join("tmp").to_string_lossy().to_string(),
            },
        )?;

        // c. Create per-step workdir
        let step_workdir = outdir.join(".steps").join(step_name);
        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: step_workdir.to_string_lossy().to_string(),
            tmpdir: step_workdir.join("tmp").to_string_lossy().to_string(),
        };

        // d. Execute the tool
        let (exit_code, outputs) = execute_tool(
            &tool,
            &step_inputs,
            &step_workdir,
            &runtime,
            &log_dir,
            step_name,
            engine,
            staging_mode,
            no_retry_copy,
        )?;

        let step_end = Utc::now();
        let container_image = parse::docker_image(&tool);

        // e. Record StepResult
        step_results.push(StepResult {
            step_name: step_name.clone(),
            tool_path: tool_path.clone(),
            container_image,
            start_time: step_start,
            end_time: step_end,
            exit_code,
            inputs: step_inputs,
            outputs: outputs.clone(),
            stdout_path: Some(log_dir.join(format!("{}.stdout.log", step_name))),
            stderr_path: Some(log_dir.join(format!("{}.stderr.log", step_name))),
        });

        // f. If non-zero exit, mark as failed and break
        if exit_code != 0 {
            success = false;
            break;
        }

        // g. Store step outputs for downstream steps
        step_outputs.insert(step_name.clone(), outputs);
    }

    // 4. Resolve workflow-level outputs from step outputs
    let mut wf_outputs = HashMap::new();
    for (out_name, out_def) in &workflow.outputs {
        if let Some(ref source) = out_def.output_source {
            let resolved = resolve_source(source, &inputs, &step_outputs);
            wf_outputs.insert(out_name.clone(), resolved);
        } else {
            wf_outputs.insert(out_name.clone(), ResolvedValue::Null);
        }
    }

    // 5. Copy final output files to outdir root and update paths
    let mut output_updates: Vec<(String, ResolvedValue)> = Vec::new();
    for (name, val) in &wf_outputs {
        if let ResolvedValue::File(fv) = val {
            let src = Path::new(&fv.path);
            if src.exists() {
                let dest = outdir.join(&fv.basename);
                // Only copy if source and dest differ
                if src != dest {
                    let _ = fs::copy(src, &dest);
                }
                if dest.exists() {
                    let updated = FileValue::from_path(&dest.to_string_lossy());
                    output_updates.push((name.clone(), ResolvedValue::File(updated)));
                }
            }
        }
    }
    for (name, val) in output_updates {
        wf_outputs.insert(name, val);
    }

    let end_time = Utc::now();

    Ok(RunResult {
        workflow_path: workflow_path.to_path_buf(),
        workflow: workflow.clone(),
        inputs,
        outputs: wf_outputs,
        steps: step_results,
        start_time,
        end_time,
        success,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve a source reference to a value.
///
/// If the source contains `/`, look up step_outputs[step_name][output_name].
/// Otherwise, look up wf_inputs[name].
fn resolve_source(
    source: &str,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
) -> ResolvedValue {
    if let Some(slash_pos) = source.find('/') {
        let step_name = &source[..slash_pos];
        let output_name = &source[slash_pos + 1..];
        step_outputs
            .get(step_name)
            .and_then(|outputs| outputs.get(output_name))
            .cloned()
            .unwrap_or(ResolvedValue::Null)
    } else {
        wf_inputs
            .get(source)
            .cloned()
            .unwrap_or(ResolvedValue::Null)
    }
}

/// Resolve step inputs from workflow inputs and upstream step outputs.
fn resolve_step_inputs(
    step_in: &HashMap<String, StepInput>,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
    runtime: &RuntimeContext,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut resolved = HashMap::new();

    for (input_name, step_input) in step_in {
        let value = match step_input {
            StepInput::Source(source) => resolve_source(source, wf_inputs, step_outputs),
            StepInput::Structured(entry) => {
                // Get base value from source or default
                let base_value = if let Some(ref source) = entry.source {
                    let v = resolve_source(source, wf_inputs, step_outputs);
                    if matches!(v, ResolvedValue::Null) {
                        // Fall back to default if source resolves to null
                        entry
                            .default
                            .as_ref()
                            .map(yaml_to_resolved)
                            .unwrap_or(ResolvedValue::Null)
                    } else {
                        v
                    }
                } else {
                    entry
                        .default
                        .as_ref()
                        .map(yaml_to_resolved)
                        .unwrap_or(ResolvedValue::Null)
                };

                // If value_from is set, resolve it using wf_inputs as the
                // inputs context (so $(inputs.x) references work).
                if let Some(ref vf) = entry.value_from {
                    let resolved_str =
                        param::resolve_param_refs(vf, wf_inputs, runtime, Some(&base_value));
                    ResolvedValue::String(resolved_str)
                } else {
                    base_value
                }
            }
        };

        resolved.insert(input_name.clone(), value);
    }

    Ok(resolved)
}

/// Convert serde_yaml::Value to ResolvedValue for handling defaults.
fn yaml_to_resolved(val: &serde_yaml::Value) -> ResolvedValue {
    match val {
        serde_yaml::Value::Null => ResolvedValue::Null,
        serde_yaml::Value::Bool(b) => ResolvedValue::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ResolvedValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                ResolvedValue::Float(f)
            } else {
                ResolvedValue::Null
            }
        }
        serde_yaml::Value::String(s) => ResolvedValue::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            let items = seq.iter().map(yaml_to_resolved).collect();
            ResolvedValue::Array(items)
        }
        serde_yaml::Value::Mapping(map) => {
            // Check for class: File or class: Directory
            let class = map
                .get(serde_yaml::Value::String("class".to_string()))
                .and_then(|v| v.as_str());
            match class {
                Some("File") => {
                    let path = map
                        .get(serde_yaml::Value::String("path".to_string()))
                        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    ResolvedValue::File(FileValue::from_path(path))
                }
                Some("Directory") => {
                    let path = map
                        .get(serde_yaml::Value::String("path".to_string()))
                        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    ResolvedValue::Directory(FileValue::from_path(path))
                }
                _ => ResolvedValue::Null,
            }
        }
        _ => ResolvedValue::Null,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ResolvedValue, StepInput, StepInputEntry};

    /// Helper: build workflow inputs for testing resolve_source.
    fn test_wf_inputs() -> HashMap<String, ResolvedValue> {
        let mut inputs = HashMap::new();
        inputs.insert(
            "sample_id".to_string(),
            ResolvedValue::String("SRX123".to_string()),
        );
        inputs.insert(
            "message".to_string(),
            ResolvedValue::String("hello".to_string()),
        );
        inputs
    }

    /// Helper: build step outputs for testing resolve_source.
    fn test_step_outputs() -> HashMap<String, HashMap<String, ResolvedValue>> {
        let mut step_outputs = HashMap::new();
        let mut align_outputs = HashMap::new();
        align_outputs.insert(
            "aligned_sam".to_string(),
            ResolvedValue::File(FileValue {
                path: "/work/aligned.sam".to_string(),
                basename: "aligned.sam".to_string(),
                nameroot: "aligned".to_string(),
                nameext: ".sam".to_string(),
                size: 4096,
                secondary_files: Vec::new(),
            }),
        );
        step_outputs.insert("align".to_string(), align_outputs);
        step_outputs
    }

    // -- Test 1: resolve_source for workflow input ----------------------------

    #[test]
    fn resolve_source_workflow_input() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("sample_id", &wf_inputs, &step_outputs);
        match result {
            ResolvedValue::String(s) => assert_eq!(s, "SRX123"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    // -- Test 2: resolve_source for step output ------------------------------

    #[test]
    fn resolve_source_step_output() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("align/aligned_sam", &wf_inputs, &step_outputs);
        match result {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.path, "/work/aligned.sam");
                assert_eq!(fv.basename, "aligned.sam");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    // -- Test 3: resolve_step_inputs with default ----------------------------

    #[test]
    fn resolve_step_inputs_with_default() {
        let wf_inputs = HashMap::new(); // no wf inputs provided
        let step_outputs = HashMap::new(); // no upstream outputs

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "threads".to_string(),
            StepInput::Structured(StepInputEntry {
                source: None,
                value_from: None,
                default: Some(serde_yaml::Value::Number(serde_yaml::Number::from(4))),
            }),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();
        match resolved.get("threads") {
            Some(ResolvedValue::Int(n)) => assert_eq!(*n, 4),
            other => panic!("expected Int(4), got {:?}", other),
        }
    }

    // -- Test 4: resolve_step_inputs with value_from -------------------------

    #[test]
    fn resolve_step_inputs_with_value_from() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = HashMap::new();

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "greeting".to_string(),
            StepInput::Structured(StepInputEntry {
                source: Some("message".to_string()),
                value_from: Some("prefix_$(self)".to_string()),
                default: None,
            }),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();
        match resolved.get("greeting") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "prefix_hello"),
            other => panic!("expected String(\"prefix_hello\"), got {:?}", other),
        }
    }

    // -- Test 5: resolve_source for missing source returns Null --------------

    #[test]
    fn resolve_source_missing_returns_null() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("nonexistent", &wf_inputs, &step_outputs);
        assert!(matches!(result, ResolvedValue::Null));

        let result = resolve_source("missing_step/output", &wf_inputs, &step_outputs);
        assert!(matches!(result, ResolvedValue::Null));
    }

    // -- Test 6: yaml_to_resolved converts various types --------------------

    #[test]
    fn yaml_to_resolved_types() {
        // String
        let v = serde_yaml::Value::String("hello".to_string());
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::String(s) if s == "hello"));

        // Int
        let v = serde_yaml::Value::Number(serde_yaml::Number::from(42));
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Int(42)));

        // Float
        let v = serde_yaml::Value::Number(serde_yaml::Number::from(3.14));
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Float(f) if (f - 3.14).abs() < 1e-10));

        // Bool
        let v = serde_yaml::Value::Bool(true);
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Bool(true)));

        // Null
        let v = serde_yaml::Value::Null;
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Null));

        // Array
        let v = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("a".to_string()),
            serde_yaml::Value::String("b".to_string()),
        ]);
        match yaml_to_resolved(&v) {
            ResolvedValue::Array(arr) => assert_eq!(arr.len(), 2),
            other => panic!("expected Array, got {:?}", other),
        }
    }

    // -- Test 7: resolve_step_inputs with Source ----------------------------

    #[test]
    fn resolve_step_inputs_simple_source() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "msg".to_string(),
            StepInput::Source("message".to_string()),
        );
        step_in.insert(
            "aligned".to_string(),
            StepInput::Source("align/aligned_sam".to_string()),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();

        match resolved.get("msg") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "hello"),
            other => panic!("expected String(\"hello\"), got {:?}", other),
        }

        match resolved.get("aligned") {
            Some(ResolvedValue::File(fv)) => assert_eq!(fv.path, "/work/aligned.sam"),
            other => panic!("expected File, got {:?}", other),
        }
    }
}
