use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{CommandLineTool, CwlDocument};

/// Parse a CWL document from a file path.
pub fn parse_cwl(path: &Path) -> Result<CwlDocument> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_cwl_str(&content)
}

/// Parse a CWL document from a string.
///
/// Strips a leading `#!/usr/bin/env cwl-runner` shebang line if present.
pub fn parse_cwl_str(content: &str) -> Result<CwlDocument> {
    let content = if content.starts_with("#!") {
        // Strip the shebang line
        match content.find('\n') {
            Some(pos) => &content[pos + 1..],
            None => "",
        }
    } else {
        content
    };
    let doc: CwlDocument =
        serde_yaml::from_str(content).context("failed to parse CWL document")?;
    Ok(doc)
}

/// Extract the Docker image from a CommandLineTool's requirements or hints.
///
/// Looks for `DockerRequirement` with a `dockerPull` field.
pub fn docker_image(tool: &CommandLineTool) -> Option<String> {
    // Check requirements first, then hints
    for entry in tool.requirements.iter().chain(tool.hints.iter()) {
        if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
            if class == "DockerRequirement" {
                if let Some(pull) = entry.get("dockerPull").and_then(|v| v.as_str()) {
                    return Some(pull.to_string());
                }
            }
        }
    }
    None
}

/// Check whether a CommandLineTool has ShellCommandRequirement.
pub fn has_shell_requirement(tool: &CommandLineTool) -> bool {
    for entry in tool.requirements.iter() {
        if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
            if class == "ShellCommandRequirement" {
                return true;
            }
        }
    }
    false
}

/// Extract ResourceRequirement coresMin and ramMin from a CommandLineTool.
///
/// Returns `(coresMin, ramMin)` with defaults of `(1, 1024)`.
pub fn resource_requirement(tool: &CommandLineTool) -> (u32, u64) {
    let mut cores: u32 = 1;
    let mut ram: u64 = 1024;
    for entry in tool.requirements.iter().chain(tool.hints.iter()) {
        if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
            if class == "ResourceRequirement" {
                if let Some(c) = entry.get("coresMin") {
                    if let Some(n) = c.as_u64() {
                        cores = n as u32;
                    } else if let Some(n) = c.as_f64() {
                        cores = n as u32;
                    }
                }
                if let Some(r) = entry.get("ramMin") {
                    if let Some(n) = r.as_u64() {
                        ram = n;
                    } else if let Some(n) = r.as_f64() {
                        ram = n as u64;
                    }
                }
                break;
            }
        }
    }
    (cores, ram)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn parse_echo_tool() {
        let doc = parse_cwl(&fixture_path("echo.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                // baseCommand is "echo"
                assert!(
                    matches!(&tool.base_command, BaseCommand::Single(s) if s == "echo"),
                    "expected baseCommand 'echo', got {:?}",
                    tool.base_command,
                );
                // inputs contains "message" with type "string"
                let msg = tool.inputs.get("message").expect("missing input 'message'");
                assert_eq!(msg.cwl_type.base_type(), "string");
                // inputBinding.position == 1
                let binding = msg.input_binding.as_ref().expect("missing inputBinding");
                assert_eq!(binding.position, Some(1));
                // stdout
                assert_eq!(tool.stdout, Some("output.txt".to_string()));
                // outputs contains "output" with type "stdout"
                let out = tool.outputs.get("output").expect("missing output 'output'");
                assert_eq!(out.cwl_type.base_type(), "stdout");
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_cat_tool() {
        let doc = parse_cwl(&fixture_path("cat.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                // File input
                let inp = tool
                    .inputs
                    .get("input_file")
                    .expect("missing input 'input_file'");
                assert_eq!(inp.cwl_type.base_type(), "File");
                // outputBinding.glob
                let out = tool.outputs.get("output").expect("missing output 'output'");
                let binding = out
                    .output_binding
                    .as_ref()
                    .expect("missing outputBinding");
                assert!(
                    matches!(&binding.glob, GlobPattern::Single(s) if s == "output.txt"),
                    "expected glob 'output.txt', got {:?}",
                    binding.glob,
                );
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_add_prefix_tool() {
        let doc = parse_cwl(&fixture_path("add-prefix.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                // baseCommand is empty array
                assert!(
                    matches!(&tool.base_command, BaseCommand::Array(v) if v.is_empty()),
                    "expected empty baseCommand array, got {:?}",
                    tool.base_command,
                );
                // ShellCommandRequirement
                assert!(
                    has_shell_requirement(&tool),
                    "expected ShellCommandRequirement",
                );
                // arguments has one structured entry
                assert_eq!(tool.arguments.len(), 1);
                match &tool.arguments[0] {
                    Argument::Structured(entry) => {
                        assert_eq!(entry.shell_quote, Some(false));
                        assert!(entry.value_from.is_some());
                    }
                    other => panic!("expected structured argument, got {:?}", other),
                }
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_two_step_workflow() {
        let doc = parse_cwl(&fixture_path("two-step.cwl")).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                // Two inputs
                assert!(wf.inputs.contains_key("message"));
                assert!(wf.inputs.contains_key("prefix"));
                // Two steps
                assert_eq!(wf.steps.len(), 2);
                let echo = wf
                    .steps
                    .get("echo_step")
                    .expect("missing step 'echo_step'");
                assert_eq!(echo.run, "echo.cwl");
                // echo_step/in/message is a Source (simple string)
                let msg_in = echo
                    .inputs
                    .get("message")
                    .expect("missing step input 'message'");
                assert!(
                    matches!(msg_in, StepInput::Source(s) if s == "message"),
                    "expected Source('message'), got {:?}",
                    msg_in,
                );
                // cat_step/in/input_file is a Source referencing echo_step/output
                let cat = wf
                    .steps
                    .get("cat_step")
                    .expect("missing step 'cat_step'");
                let file_in = cat
                    .inputs
                    .get("input_file")
                    .expect("missing step input 'input_file'");
                assert!(
                    matches!(file_in, StepInput::Source(s) if s == "echo_step/output"),
                    "expected Source('echo_step/output'), got {:?}",
                    file_in,
                );
                // outputSource
                let final_out = wf
                    .outputs
                    .get("final_output")
                    .expect("missing output 'final_output'");
                assert_eq!(
                    final_out.output_source,
                    Some("cat_step/output".to_string())
                );
            }
            _ => panic!("expected Workflow"),
        }
    }

    #[test]
    fn parse_docker_image() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
hints:
  - class: DockerRequirement
    dockerPull: ubuntu:22.04
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(docker_image(&tool), Some("ubuntu:22.04".to_string()));
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_resource_requirement() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  - class: ResourceRequirement
    coresMin: 4
    ramMin: 8192
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let (cores, ram) = resource_requirement(&tool);
                assert_eq!(cores, 4);
                assert_eq!(ram, 8192);
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_resource_defaults() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let (cores, ram) = resource_requirement(&tool);
                assert_eq!(cores, 1);
                assert_eq!(ram, 1024);
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_strips_shebang() {
        let yaml = "#!/usr/bin/env cwl-runner\nclass: CommandLineTool\nbaseCommand: echo\ninputs: {}\noutputs: {}\n";
        let doc = parse_cwl_str(yaml).unwrap();
        assert!(matches!(doc, CwlDocument::CommandLineTool(_)));
    }
}
