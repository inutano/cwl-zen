use std::collections::HashMap;

use crate::model::{
    Argument, BaseCommand, CommandLineTool, ResolvedValue, RuntimeContext,
};
use crate::param;
use crate::parse;

/// A fully resolved command ready for execution.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    pub command_line: String,
    pub use_shell: bool,
    pub docker_image: Option<String>,
    pub cores: u32,
    pub ram: u64,
    pub stdout_file: Option<String>,
    pub network_access: bool,
}

/// Build a resolved command from a parsed CommandLineTool, resolved inputs, and
/// runtime context.
///
/// Steps:
/// 1. Detect ShellCommandRequirement, DockerRequirement, ResourceRequirement
/// 2. Override runtime cores/ram with tool's ResourceRequirement values
/// 3. Collect command parts as (position, text) pairs
/// 4. Sort by position (stable)
/// 5. Join into a single command line string
/// 6. Resolve stdout filename if present
pub fn build_command(
    tool: &CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> ResolvedCommand {
    let use_shell = parse::has_shell_requirement(tool);
    let docker_image = parse::docker_image(tool);
    let (res_cores, res_ram) = parse::resource_requirement(tool);

    // Override runtime with tool resource requirements
    let effective_runtime = RuntimeContext {
        cores: res_cores,
        ram: res_ram,
        outdir: runtime.outdir.clone(),
        tmpdir: runtime.tmpdir.clone(),
    };

    let mut parts: Vec<(i32, String)> = Vec::new();

    // 1. baseCommand parts at position -1000
    match &tool.base_command {
        BaseCommand::Single(s) => {
            parts.push((-1000, s.clone()));
        }
        BaseCommand::Array(arr) => {
            for s in arr {
                parts.push((-1000, s.clone()));
            }
        }
        BaseCommand::None => {}
    }

    // 2. arguments
    for arg in &tool.arguments {
        match arg {
            Argument::String(s) => {
                let resolved =
                    param::resolve_param_refs(s, inputs, &effective_runtime, None);
                parts.push((0, resolved));
            }
            Argument::Structured(entry) => {
                let position = entry.position.unwrap_or(0);
                let value = if let Some(ref vf) = entry.value_from {
                    param::resolve_param_refs(vf, inputs, &effective_runtime, None)
                } else {
                    String::new()
                };
                let text = if let Some(ref prefix) = entry.prefix {
                    if value.is_empty() {
                        prefix.clone()
                    } else {
                        format!("{} {}", prefix, value)
                    }
                } else {
                    value
                };
                // Trim trailing whitespace (valueFrom may have trailing newline
                // from YAML literal blocks)
                let text = text.trim_end().to_string();
                parts.push((position, text));
            }
        }
    }

    // 3. inputs with inputBinding
    // Sort input names for deterministic ordering of same-position entries
    let mut input_names: Vec<&String> = inputs.keys().collect();
    input_names.sort();

    for name in &input_names {
        let tool_input = match tool.inputs.get(name.as_str()) {
            Some(ti) => ti,
            None => continue,
        };
        let binding = match &tool_input.input_binding {
            Some(b) => b,
            None => continue,
        };

        let val = match inputs.get(name.as_str()) {
            Some(v) => v,
            None => continue,
        };

        // Skip null/absent optional inputs
        if matches!(val, ResolvedValue::Null) {
            continue;
        }

        let position = binding.position.unwrap_or(0);

        // Resolve the value: use valueFrom if present, otherwise stringify the value
        let value_str = if let Some(ref vf) = binding.value_from {
            param::resolve_param_refs(vf, inputs, &effective_runtime, Some(val))
        } else {
            param::value_to_string(val)
        };

        let separate = binding.separate.unwrap_or(true);

        let text = if let Some(ref prefix) = binding.prefix {
            if separate {
                format!("{} {}", prefix, value_str)
            } else {
                format!("{}{}", prefix, value_str)
            }
        } else {
            value_str
        };

        parts.push((position, text));
    }

    // 4. Sort by position (stable sort preserves insertion order for ties)
    parts.sort_by_key(|&(pos, _)| pos);

    // 5. Join
    let command_line = parts
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join(" ");

    // 6. Resolve stdout
    let stdout_file = tool
        .stdout
        .as_ref()
        .map(|s| param::resolve_param_refs(s, inputs, &effective_runtime, None));

    // 7. Detect NetworkAccess requirement
    let network_access = crate::parse::has_network_access(tool);

    ResolvedCommand {
        command_line,
        use_shell,
        docker_image,
        cores: effective_runtime.cores,
        ram: effective_runtime.ram,
        stdout_file,
        network_access,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::parse;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn default_runtime() -> RuntimeContext {
        RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        }
    }

    fn basic_runtime() -> RuntimeContext {
        default_runtime()
    }

    #[test]
    fn build_echo_command() {
        let doc = parse::parse_cwl(&fixture_path("echo.cwl")).unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "message".to_string(),
            ResolvedValue::String("hello".to_string()),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert_eq!(cmd.command_line, "echo hello");
        assert!(!cmd.use_shell);
        assert_eq!(cmd.docker_image, None);
        assert_eq!(cmd.stdout_file, Some("output.txt".to_string()));
    }

    #[test]
    fn build_cat_command() {
        let doc = parse::parse_cwl(&fixture_path("cat.cwl")).unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "input_file".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/test.txt".to_string(),
                basename: "test.txt".to_string(),
                nameroot: "test".to_string(),
                nameext: ".txt".to_string(),
                size: 100,
                secondary_files: Vec::new(),
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert_eq!(cmd.command_line, "cat /data/test.txt");
    }

    #[test]
    fn build_shell_command() {
        let doc = parse::parse_cwl(&fixture_path("add-prefix.cwl")).unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "prefix".to_string(),
            ResolvedValue::String("sample".to_string()),
        );
        inputs.insert(
            "input_file".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/input.txt".to_string(),
                basename: "input.txt".to_string(),
                nameroot: "input".to_string(),
                nameext: ".txt".to_string(),
                size: 50,
                secondary_files: Vec::new(),
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert!(cmd.use_shell, "expected use_shell=true");
        // The valueFrom should resolve inputs references
        assert!(
            cmd.command_line.contains("sample"),
            "expected command to contain 'sample', got: {}",
            cmd.command_line
        );
        assert!(
            cmd.command_line.contains("/data/input.txt"),
            "expected command to contain '/data/input.txt', got: {}",
            cmd.command_line
        );
    }

    #[test]
    fn build_docker_command() {
        let yaml = r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: bwa
hints:
  - class: DockerRequirement
    dockerPull: "quay.io/biocontainers/bwa:0.7.17"
requirements:
  - class: ResourceRequirement
    coresMin: 8
    ramMin: 16384
inputs:
  ref:
    type: File
    inputBinding:
      position: 1
outputs: {}
"#;
        let doc = parse::parse_cwl_str(yaml).unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "ref".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/ref.fa".to_string(),
                basename: "ref.fa".to_string(),
                nameroot: "ref".to_string(),
                nameext: ".fa".to_string(),
                size: 3_000_000_000,
                secondary_files: Vec::new(),
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert_eq!(
            cmd.docker_image,
            Some("quay.io/biocontainers/bwa:0.7.17".to_string())
        );
        assert_eq!(cmd.cores, 8);
        assert_eq!(cmd.ram, 16384);
        assert_eq!(cmd.command_line, "bwa /data/ref.fa");
    }

    #[test]
    fn separate_false_no_space() {
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
inputs:
  threads:
    type: int
    inputBinding:
      prefix: "--threads="
      separate: false
      position: 1
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let mut inputs = HashMap::new();
        inputs.insert("threads".into(), ResolvedValue::Int(8));
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert!(cmd.command_line.contains("--threads=8"));
    }

    #[test]
    fn network_access_detected() {
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: curl
requirements:
  - class: NetworkAccess
    networkAccess: true
inputs: {}
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let cmd = build_command(&tool, &HashMap::new(), &basic_runtime());
        assert!(cmd.network_access);
    }

    #[test]
    fn network_access_default_false() {
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let cmd = build_command(&tool, &HashMap::new(), &basic_runtime());
        assert!(!cmd.network_access);
    }
}
