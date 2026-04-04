use std::collections::HashMap;

use crate::model::{
    Argument, BaseCommand, CommandLineTool, CwlType, ResolvedValue, RuntimeContext,
};
use crate::param;
use crate::parse;

/// A fully resolved command ready for execution.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    pub args: Vec<String>,
    pub use_shell: bool,
    pub docker_image: Option<String>,
    pub cores: u32,
    pub ram: u64,
    pub stdout_file: Option<String>,
    pub stdin_file: Option<String>,
    pub stderr_file: Option<String>,
    pub network_access: bool,
}

impl ResolvedCommand {
    /// Get command line as a single string (for shell mode or display).
    pub fn command_line(&self) -> String {
        self.args.join(" ")
    }
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

    let mut parts: Vec<(i32, Vec<String>)> = Vec::new();

    // 1. baseCommand parts at position -1000 (each as a separate token)
    match &tool.base_command {
        BaseCommand::Single(s) => {
            parts.push((-1000, vec![s.clone()]));
        }
        BaseCommand::Array(arr) => {
            for s in arr {
                parts.push((-1000, vec![s.clone()]));
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
                parts.push((0, vec![resolved]));
            }
            Argument::Structured(entry) => {
                let position = entry.position.unwrap_or(0);
                let value = if let Some(ref vf) = entry.value_from {
                    param::resolve_param_refs(vf, inputs, &effective_runtime, None)
                        .trim_end()
                        .to_string()
                } else {
                    String::new()
                };
                let tokens = if let Some(ref prefix) = entry.prefix {
                    if value.is_empty() {
                        vec![prefix.clone()]
                    } else {
                        // Arguments always separate prefix and value (CWL default)
                        vec![prefix.clone(), value]
                    }
                } else {
                    vec![value]
                };
                parts.push((position, tokens));
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

        // Resolve the value: use valueFrom if present, otherwise stringify the value.
        // Handle array values with itemSeparator: join elements with separator into
        // a single string. Without itemSeparator, each array element becomes a
        // separate arg (possibly each prefixed).
        if let ResolvedValue::Array(arr) = val {
            // Empty arrays produce no command line arguments
            if arr.is_empty() {
                continue;
            }
            if binding.value_from.is_none() {
                if let Some(ref sep) = binding.item_separator {
                    // Join array elements with separator into single value
                    let value_str = arr
                        .iter()
                        .map(param::value_to_string)
                        .collect::<Vec<_>>()
                        .join(sep);

                    let separate = binding.separate.unwrap_or(true);
                    let tokens = if let Some(ref prefix) = binding.prefix {
                        if separate {
                            vec![prefix.clone(), value_str]
                        } else {
                            vec![format!("{}{}", prefix, value_str)]
                        }
                    } else {
                        vec![value_str]
                    };
                    parts.push((position, tokens));
                    continue;
                } else {
                    // No itemSeparator: each array element is a separate arg.
                    // Check for nested inputBinding on the array type definition.
                    let inner_binding = match &tool_input.cwl_type {
                        CwlType::ArrayType { inner_binding, .. } => inner_binding.as_ref(),
                        CwlType::Union(variants) => {
                            variants.iter().find_map(|v| match v {
                                CwlType::ArrayType { inner_binding, .. } => inner_binding.as_ref(),
                                _ => None,
                            })
                        }
                        _ => None,
                    };

                    if let Some(ib) = inner_binding {
                        // Nested binding: outer prefix goes once, then each
                        // element gets the inner prefix.
                        if let Some(ref prefix) = binding.prefix {
                            parts.push((position, vec![prefix.clone()]));
                        }
                        for item in arr {
                            let item_str = param::value_to_string(item);
                            let sep = ib.separate.unwrap_or(true);
                            let tokens = if let Some(ref inner_prefix) = ib.prefix {
                                if sep {
                                    vec![inner_prefix.clone(), item_str]
                                } else {
                                    vec![format!("{}{}", inner_prefix, item_str)]
                                }
                            } else {
                                vec![item_str]
                            };
                            parts.push((position, tokens));
                        }
                    } else {
                        // No inner binding: each element gets the outer prefix
                        for item in arr {
                            let item_str = param::value_to_string(item);
                            let separate = binding.separate.unwrap_or(true);
                            let tokens = if let Some(ref prefix) = binding.prefix {
                                if separate {
                                    vec![prefix.clone(), item_str]
                                } else {
                                    vec![format!("{}{}", prefix, item_str)]
                                }
                            } else {
                                vec![item_str]
                            };
                            parts.push((position, tokens));
                        }
                    }
                    continue;
                }
            }
        }

        // Handle boolean values: true with prefix emits flag, false emits nothing,
        // true without prefix emits nothing (CWL spec).
        if let ResolvedValue::Bool(b) = val {
            if !b {
                continue; // false booleans never emit anything
            }
            if let Some(ref prefix) = binding.prefix {
                parts.push((position, vec![prefix.clone()]));
            }
            // true without prefix: emit nothing
            continue;
        }

        let value_str = if let Some(ref vf) = binding.value_from {
            param::resolve_param_refs(vf, inputs, &effective_runtime, Some(val))
        } else {
            param::value_to_string(val)
        };

        let separate = binding.separate.unwrap_or(true);

        let tokens = if let Some(ref prefix) = binding.prefix {
            if separate {
                vec![prefix.clone(), value_str]
            } else {
                vec![format!("{}{}", prefix, value_str)]
            }
        } else {
            vec![value_str]
        };

        parts.push((position, tokens));
    }

    // 4. Sort by position (stable sort preserves insertion order for ties)
    parts.sort_by_key(|(pos, _)| *pos);

    // 5. Flatten into ordered arg vector
    let args: Vec<String> = parts
        .into_iter()
        .flat_map(|(_, tokens)| tokens)
        .collect();

    // 6. Resolve stdout
    let stdout_file = tool
        .stdout
        .as_ref()
        .map(|s| param::resolve_param_refs(s, inputs, &effective_runtime, None));

    // 7. Resolve stdin
    let stdin_file = tool
        .stdin
        .as_ref()
        .map(|s| param::resolve_param_refs(s, inputs, &effective_runtime, None));

    // 8. Resolve stderr
    let stderr_file = tool
        .stderr
        .as_ref()
        .map(|s| param::resolve_param_refs(s, inputs, &effective_runtime, None));

    // 9. Detect NetworkAccess requirement
    let network_access = crate::parse::has_network_access(tool);

    ResolvedCommand {
        args,
        use_shell,
        docker_image,
        cores: effective_runtime.cores,
        ram: effective_runtime.ram,
        stdout_file,
        stdin_file,
        stderr_file,
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
        assert_eq!(cmd.command_line(), "echo hello");
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
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert_eq!(cmd.command_line(), "cat /data/test.txt");
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
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert!(cmd.use_shell, "expected use_shell=true");
        // The valueFrom should resolve inputs references
        assert!(
            cmd.command_line().contains("sample"),
            "expected command to contain 'sample', got: {}",
            cmd.command_line()
        );
        assert!(
            cmd.command_line().contains("/data/input.txt"),
            "expected command to contain '/data/input.txt', got: {}",
            cmd.command_line()
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
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );

        let cmd = build_command(&tool, &inputs, &default_runtime());
        assert_eq!(
            cmd.docker_image,
            Some("quay.io/biocontainers/bwa:0.7.17".to_string())
        );
        assert_eq!(cmd.cores, 8);
        assert_eq!(cmd.ram, 16384);
        assert_eq!(cmd.command_line(), "bwa /data/ref.fa");
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
        assert!(cmd.command_line().contains("--threads=8"));
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

    #[test]
    fn args_preserved_as_vector() {
        // Tool with baseCommand: [bwa, mem] and a prefixed input with separate=true
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: [bwa, mem]
inputs:
  threads:
    type: int
    inputBinding:
      prefix: "-t"
      position: 1
  min_seed:
    type: int
    inputBinding:
      prefix: "-m"
      position: 2
  reference:
    type: File
    inputBinding:
      position: 3
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };

        let mut inputs = HashMap::new();
        inputs.insert("threads".to_string(), ResolvedValue::Int(4));
        inputs.insert("min_seed".to_string(), ResolvedValue::Int(3));
        inputs.insert(
            "reference".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/ref.fa".to_string(),
                basename: "ref.fa".to_string(),
                nameroot: "ref".to_string(),
                nameext: ".fa".to_string(),
                size: 1000,
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );

        let cmd = build_command(&tool, &inputs, &basic_runtime());

        // Args should be individual tokens, not a single joined string
        assert_eq!(
            cmd.args,
            vec!["bwa", "mem", "-t", "4", "-m", "3", "/data/ref.fa"]
        );
        // command_line() still works for display
        assert_eq!(cmd.command_line(), "bwa mem -t 4 -m 3 /data/ref.fa");
    }

    #[test]
    fn args_separate_false_single_token() {
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

        // With separate=false, prefix and value should be a single token
        assert_eq!(cmd.args, vec!["tool", "--threads=8"]);
    }

    #[test]
    fn item_separator_joins_array() {
        // Array input with itemSeparator should produce a single joined value
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
inputs:
  values:
    type:
      type: array
      items: int
    inputBinding:
      prefix: "-I"
      position: 1
      itemSeparator: ","
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let mut inputs = HashMap::new();
        inputs.insert(
            "values".to_string(),
            ResolvedValue::Array(vec![
                ResolvedValue::Int(1),
                ResolvedValue::Int(2),
                ResolvedValue::Int(3),
                ResolvedValue::Int(4),
            ]),
        );
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.args, vec!["tool", "-I", "1,2,3,4"]);
    }

    #[test]
    fn array_without_item_separator_separate_args() {
        // Array input without itemSeparator: each element is a separate arg
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
inputs:
  files:
    type:
      type: array
      items: string
    inputBinding:
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
        inputs.insert(
            "files".to_string(),
            ResolvedValue::Array(vec![
                ResolvedValue::String("a.txt".to_string()),
                ResolvedValue::String("b.txt".to_string()),
            ]),
        );
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.args, vec!["tool", "a.txt", "b.txt"]);
    }

    #[test]
    fn stdin_resolved_from_tool() {
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: cat
inputs:
  file1:
    type: File
outputs: {}
stdin: $(inputs.file1.path)
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let mut inputs = HashMap::new();
        inputs.insert(
            "file1".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/input.txt".to_string(),
                basename: "input.txt".to_string(),
                nameroot: "input".to_string(),
                nameext: ".txt".to_string(),
                size: 100,
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.stdin_file, Some("/data/input.txt".to_string()));
    }

    #[test]
    fn stderr_resolved_from_tool() {
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
stderr: error.log
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let cmd = build_command(&tool, &HashMap::new(), &basic_runtime());
        assert_eq!(cmd.stderr_file, Some("error.log".to_string()));
    }

    #[test]
    fn nested_prefix_array_type() {
        // Test nested inputBinding on array type: outer prefix once, inner prefix per element
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
arguments: ["bwa", "mem"]
inputs:
  reference:
    type: File
    inputBinding:
      position: 2
  reads:
    type:
      type: array
      items: File
      inputBinding:
        prefix: "-YYY"
    inputBinding:
      position: 3
      prefix: "-XXX"
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let mut inputs = HashMap::new();
        inputs.insert(
            "reference".to_string(),
            ResolvedValue::File(FileValue {
                path: "chr20.fa".to_string(),
                basename: "chr20.fa".to_string(),
                nameroot: "chr20".to_string(),
                nameext: ".fa".to_string(),
                size: 123,
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );
        inputs.insert(
            "reads".to_string(),
            ResolvedValue::Array(vec![
                ResolvedValue::File(FileValue {
                    path: "file1.fastq".to_string(),
                    basename: "file1.fastq".to_string(),
                    nameroot: "file1".to_string(),
                    nameext: ".fastq".to_string(),
                    size: 100,
                    checksum: None,
                    secondary_files: Vec::new(),
                    contents: None,
                    format: None,
                }),
                ResolvedValue::File(FileValue {
                    path: "file2.fastq".to_string(),
                    basename: "file2.fastq".to_string(),
                    nameroot: "file2".to_string(),
                    nameext: ".fastq".to_string(),
                    size: 200,
                    checksum: None,
                    secondary_files: Vec::new(),
                    contents: None,
                    format: None,
                }),
            ]),
        );
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        // Expected: tool bwa mem chr20.fa -XXX -YYY file1.fastq -YYY file2.fastq
        assert_eq!(
            cmd.args,
            vec![
                "tool", "bwa", "mem", "chr20.fa",
                "-XXX",
                "-YYY", "file1.fastq",
                "-YYY", "file2.fastq",
            ]
        );
    }

    #[test]
    fn nested_prefix_array_no_inner_binding() {
        // Without inner binding, outer prefix applies per element (existing behavior)
        let doc = crate::parse::parse_cwl_str(
            r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
inputs:
  files:
    type:
      type: array
      items: File
    inputBinding:
      position: 1
      prefix: "-f"
outputs: {}
"#,
        )
        .unwrap();
        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => panic!("expected CommandLineTool"),
        };
        let mut inputs = HashMap::new();
        inputs.insert(
            "files".to_string(),
            ResolvedValue::Array(vec![
                ResolvedValue::File(FileValue {
                    path: "a.txt".to_string(),
                    basename: "a.txt".to_string(),
                    nameroot: "a".to_string(),
                    nameext: ".txt".to_string(),
                    size: 10,
                    checksum: None,
                    secondary_files: Vec::new(),
                    contents: None,
                    format: None,
                }),
                ResolvedValue::File(FileValue {
                    path: "b.txt".to_string(),
                    basename: "b.txt".to_string(),
                    nameroot: "b".to_string(),
                    nameext: ".txt".to_string(),
                    size: 20,
                    checksum: None,
                    secondary_files: Vec::new(),
                    contents: None,
                    format: None,
                }),
            ]),
        );
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        // Without inner binding: each element gets the outer prefix
        assert_eq!(cmd.args, vec!["tool", "-f", "a.txt", "-f", "b.txt"]);
    }
}
