use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Top-level CWL document
// ---------------------------------------------------------------------------

/// A CWL document is either a CommandLineTool or a Workflow.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "class")]
pub enum CwlDocument {
    CommandLineTool(CommandLineTool),
    Workflow(Workflow),
}

// ---------------------------------------------------------------------------
// CommandLineTool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLineTool {
    #[serde(default)]
    pub cwl_version: Option<String>,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub base_command: BaseCommand,

    #[serde(default)]
    pub arguments: Vec<Argument>,

    #[serde(default)]
    pub inputs: HashMap<String, ToolInput>,

    #[serde(default)]
    pub outputs: HashMap<String, ToolOutput>,

    #[serde(default)]
    pub requirements: Vec<serde_yaml::Value>,

    #[serde(default)]
    pub hints: Vec<serde_yaml::Value>,

    #[serde(default)]
    pub stdout: Option<String>,
}

// ---------------------------------------------------------------------------
// BaseCommand — string, array of strings, or absent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BaseCommand {
    Single(String),
    Array(Vec<String>),
    None,
}

impl Default for BaseCommand {
    fn default() -> Self {
        BaseCommand::None
    }
}

// ---------------------------------------------------------------------------
// Argument — plain string or structured entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Argument {
    String(String),
    Structured(ArgumentEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgumentEntry {
    #[serde(default)]
    pub prefix: Option<String>,

    #[serde(default)]
    pub value_from: Option<String>,

    #[serde(default)]
    pub position: Option<i32>,

    #[serde(default)]
    pub shell_quote: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInput {
    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub input_binding: Option<InputBinding>,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputBinding {
    #[serde(default)]
    pub position: Option<i32>,

    #[serde(default)]
    pub prefix: Option<String>,

    #[serde(default)]
    pub separate: Option<bool>,

    #[serde(default)]
    pub shell_quote: Option<bool>,

    #[serde(default)]
    pub value_from: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub output_binding: Option<OutputBinding>,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputBinding {
    #[serde(default)]
    pub glob: GlobPattern,
}

// ---------------------------------------------------------------------------
// GlobPattern — single string, array, or absent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum GlobPattern {
    Single(String),
    Array(Vec<String>),
    None,
}

impl Default for GlobPattern {
    fn default() -> Self {
        GlobPattern::None
    }
}

// ---------------------------------------------------------------------------
// CwlType — single string or array of strings (for union types)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CwlType {
    Single(String),
    Array(Vec<String>),
}

impl CwlType {
    /// Return the base type name, stripping optional "?" suffix and "[]" array
    /// suffix. For union arrays like `["null", "File"]`, return the first
    /// non-"null" element.
    pub fn base_type(&self) -> &str {
        match self {
            CwlType::Single(s) => {
                let s = s.trim_end_matches('?');
                let s = s.trim_end_matches("[]");
                s
            }
            CwlType::Array(v) => {
                for item in v {
                    if item != "null" {
                        return item.as_str();
                    }
                }
                // Degenerate case: all null
                "null"
            }
        }
    }

    /// Returns true if the type is optional (nullable).
    /// - `"File?"` -> true
    /// - `["null", "File"]` -> true
    /// - `"File"` -> false
    pub fn is_optional(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with('?'),
            CwlType::Array(v) => v.iter().any(|item| item == "null"),
        }
    }

    /// Returns true if the type represents an array type (e.g. `"File[]"`).
    pub fn is_array(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with("[]"),
            CwlType::Array(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// SecondaryFile — plain pattern string or structured entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SecondaryFile {
    Pattern(String),
    Structured(SecondaryFileEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecondaryFileEntry {
    pub pattern: String,

    #[serde(default)]
    pub required: Option<bool>,
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    #[serde(default)]
    pub cwl_version: Option<String>,

    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub inputs: HashMap<String, WorkflowInput>,

    #[serde(default)]
    pub outputs: HashMap<String, WorkflowOutput>,

    #[serde(default)]
    pub steps: HashMap<String, WorkflowStep>,

    #[serde(default)]
    pub requirements: Vec<serde_yaml::Value>,
}

// ---------------------------------------------------------------------------
// Workflow inputs / outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInput {
    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub secondary_files: Vec<SecondaryFile>,

    #[serde(default)]
    pub doc: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOutput {
    #[serde(rename = "type")]
    pub cwl_type: CwlType,

    #[serde(default)]
    pub output_source: Option<String>,

    #[serde(default)]
    pub doc: Option<String>,
}

// ---------------------------------------------------------------------------
// Workflow steps
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    pub run: String,

    #[serde(rename = "in", default)]
    pub inputs: HashMap<String, StepInput>,

    #[serde(default)]
    pub out: Vec<String>,

    #[serde(default)]
    pub scatter: Option<ScatterField>,

    #[serde(default)]
    pub scatter_method: Option<String>,
}

/// A step input can be a simple source string or a structured entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepInput {
    Source(String),
    Structured(StepInputEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepInputEntry {
    #[serde(default)]
    pub source: Option<String>,

    #[serde(default)]
    pub value_from: Option<String>,

    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

/// Scatter can target a single input or multiple inputs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ScatterField {
    Single(String),
    Multiple(Vec<String>),
}

// ---------------------------------------------------------------------------
// Runtime / resolved value types
// ---------------------------------------------------------------------------

/// A fully resolved input/output value at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    File(FileValue),
    Directory(FileValue),
    Array(Vec<ResolvedValue>),
    Null,
}

/// Represents a CWL File or Directory value with computed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileValue {
    pub path: String,
    pub basename: String,
    pub nameroot: String,
    pub nameext: String,
    pub size: u64,
    pub secondary_files: Vec<FileValue>,
}

impl FileValue {
    /// Build a `FileValue` from a filesystem path. The file does not need to
    /// exist (size will be 0 if it cannot be read).
    pub fn from_path(p: &str) -> Self {
        let path = Path::new(p);
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let nameext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let nameroot = if nameext.is_empty() {
            basename.clone()
        } else {
            basename
                .strip_suffix(&nameext)
                .unwrap_or(&basename)
                .to_string()
        };
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        FileValue {
            path: p.to_string(),
            basename,
            nameroot,
            nameext,
            size,
            secondary_files: Vec::new(),
        }
    }
}

/// Runtime resource context passed to expressions and the command builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeContext {
    pub cores: u32,
    pub ram: u64,
    pub outdir: String,
    pub tmpdir: String,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- CwlType::base_type() -----------------------------------------------

    #[test]
    fn base_type_plain() {
        let t = CwlType::Single("File".to_string());
        assert_eq!(t.base_type(), "File");
    }

    #[test]
    fn base_type_optional() {
        let t = CwlType::Single("File?".to_string());
        assert_eq!(t.base_type(), "File");
    }

    #[test]
    fn base_type_array_suffix() {
        let t = CwlType::Single("string[]".to_string());
        assert_eq!(t.base_type(), "string");
    }

    #[test]
    fn base_type_union_array() {
        let t = CwlType::Array(vec!["null".to_string(), "File".to_string()]);
        assert_eq!(t.base_type(), "File");
    }

    // -- CwlType::is_optional() ---------------------------------------------

    #[test]
    fn is_optional_plain() {
        let t = CwlType::Single("File".to_string());
        assert!(!t.is_optional());
    }

    #[test]
    fn is_optional_question_mark() {
        let t = CwlType::Single("File?".to_string());
        assert!(t.is_optional());
    }

    #[test]
    fn is_optional_union_with_null() {
        let t = CwlType::Array(vec!["null".to_string(), "File".to_string()]);
        assert!(t.is_optional());
    }

    // -- CwlType::is_array() ------------------------------------------------

    #[test]
    fn is_array_plain() {
        let t = CwlType::Single("File".to_string());
        assert!(!t.is_array());
    }

    #[test]
    fn is_array_bracket_suffix() {
        let t = CwlType::Single("File[]".to_string());
        assert!(t.is_array());
    }

    // -- FileValue::from_path() ---------------------------------------------

    #[test]
    fn file_value_from_path() {
        let fv = FileValue::from_path("/data/reads.fastq.gz");
        assert_eq!(fv.basename, "reads.fastq.gz");
        assert_eq!(fv.nameext, ".gz");
        assert_eq!(fv.nameroot, "reads.fastq");
        assert_eq!(fv.path, "/data/reads.fastq.gz");
        // File doesn't exist, so size should be 0
        assert_eq!(fv.size, 0);
    }

    #[test]
    fn file_value_no_extension() {
        let fv = FileValue::from_path("/usr/bin/bash");
        assert_eq!(fv.basename, "bash");
        assert_eq!(fv.nameext, "");
        assert_eq!(fv.nameroot, "bash");
    }

    // -- Serde round-trip for CommandLineTool --------------------------------

    #[test]
    fn deserialize_command_line_tool() {
        let yaml = r#"
class: CommandLineTool
cwlVersion: v1.2
baseCommand: echo
inputs:
  message:
    type: string
    inputBinding:
      position: 1
outputs:
  out:
    type: stdout
stdout: output.txt
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(matches!(tool.base_command, BaseCommand::Single(ref s) if s == "echo"));
                assert!(tool.inputs.contains_key("message"));
                assert_eq!(tool.stdout, Some("output.txt".to_string()));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    // -- Serde round-trip for Workflow ---------------------------------------

    #[test]
    fn deserialize_workflow() {
        let yaml = r#"
class: Workflow
cwlVersion: v1.2
inputs:
  infile:
    type: File
outputs:
  outfile:
    type: File
    outputSource: step1/result
steps:
  step1:
    run: tool.cwl
    in:
      input1: infile
    out: [result]
"#;
        let doc: CwlDocument = serde_yaml::from_str(yaml).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                assert!(wf.inputs.contains_key("infile"));
                assert!(wf.outputs.contains_key("outfile"));
                assert!(wf.steps.contains_key("step1"));
                let step = &wf.steps["step1"];
                assert_eq!(step.run, "tool.cwl");
                assert_eq!(step.out, vec!["result"]);
            }
            _ => panic!("Expected Workflow"),
        }
    }
}
