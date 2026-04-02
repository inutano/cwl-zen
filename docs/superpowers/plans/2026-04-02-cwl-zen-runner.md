# CWL Zen Runner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a minimal CWL v1.2 runner (JS-free subset) in Rust with built-in Provenance Run Crate generation.

**Architecture:** YAML parsing (`serde_yaml`) → typed model → DAG construction (topological sort) → step execution (Docker/Singularity via `std::process::Command`) → output collection (glob) → RO-Crate JSON-LD generation (`serde_json`). Single binary, no JS engine.

**Tech Stack:** Rust, serde/serde_yaml/serde_json, clap, sha2, chrono, uuid, glob

**Specs:**
- `docs/spec.md` — CWL Zen language spec
- `docs/superpowers/specs/2026-04-02-provenance-ro-crate-design.md` — RO-Crate provenance design
- Reference pipeline: `~/repos/chip-atlas-pipeline-v2/cwl/` (15 tools, 5 workflows)

---

## File Structure

```
cwl-zen/
  Cargo.toml
  src/
    main.rs          — CLI (clap): run, validate, lint, dag subcommands
    lib.rs           — public re-exports
    model.rs         — CWL type definitions (CwlDocument, CommandLineTool, Workflow, etc.)
    parse.rs         — YAML → model structs
    param.rs         — parameter reference resolution: $(inputs.X), $(runtime.X), $(self)
    input.rs         — input YAML parsing and resolution
    dag.rs           — step dependency graph, topological sort
    command.rs       — build Docker/Singularity commands from tool definitions
    execute.rs       — run steps, capture stdout/stderr, timing, RunResult
    stage.rs         — file staging (mounts) and output collection (glob)
    scatter.rs       — scatter/dotproduct expansion
    provenance.rs    — RO-Crate Provenance Run Crate generation
  tests/
    fixtures/
      echo.cwl              — minimal CommandLineTool for testing
      cat.cwl               — tool with File input/output
      add-prefix.cwl        — tool with ShellCommandRequirement + valueFrom
      two-step.cwl          — minimal 2-step workflow
      scatter-workflow.cwl  — workflow with scatter/dotproduct
      echo-input.yml        — input for echo.cwl
      cat-input.yml         — input for cat.cwl
      two-step-input.yml    — input for two-step.cwl
```

---

## Task 1: Project Scaffold and CWL Data Model

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/model.rs`
- Test: `src/model.rs` (unit tests)

- [ ] **Step 1: Initialize Cargo project**

```bash
cd ~/repos/cwl-zen
cargo init --name cwl-zen
```

- [ ] **Step 2: Add dependencies to Cargo.toml**

```toml
[package]
name = "cwl-zen"
version = "0.1.0"
edition = "2021"
description = "A minimal, JS-free CWL v1.2 runner with built-in provenance"
license = "MIT"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1"
clap = { version = "4", features = ["derive"] }
sha2 = "0.10"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
glob = "0.3"
anyhow = "1"
```

- [ ] **Step 3: Write the CWL data model with tests**

Write `src/model.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Top-level CWL document — either a CommandLineTool or a Workflow.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "class")]
pub enum CwlDocument {
    CommandLineTool(CommandLineTool),
    Workflow(Workflow),
}

/// A CWL CommandLineTool definition.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLineTool {
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
    pub requirements: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub hints: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub stdout: Option<String>,
}

/// baseCommand can be a single string or an array of strings.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum BaseCommand {
    Single(String),
    Array(Vec<String>),
    #[default]
    None,
}

/// An entry in the `arguments` array.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Argument {
    /// A plain string argument: `- "-v"`
    String(String),
    /// A structured argument with prefix/valueFrom/position/shellQuote.
    Structured(ArgumentEntry),
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
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

/// A tool input definition.
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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
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

/// A tool output definition.
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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct OutputBinding {
    #[serde(default)]
    pub glob: GlobPattern,
}

/// glob can be a single string or an array of strings.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum GlobPattern {
    Single(String),
    Array(Vec<String>),
    #[default]
    None,
}

/// CWL type — can be a simple string like "File" or "File?" or "string[]".
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CwlType {
    Single(String),
    Array(Vec<String>),
}

impl CwlType {
    /// Returns the base type name, stripping ? and [] suffixes.
    pub fn base_type(&self) -> &str {
        match self {
            CwlType::Single(s) => s.trim_end_matches('?').trim_end_matches("[]"),
            CwlType::Array(arr) => {
                // Union type: find first non-null type
                arr.iter()
                    .find(|t| t.as_str() != "null")
                    .map(|s| s.as_str())
                    .unwrap_or("null")
            }
        }
    }

    pub fn is_optional(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with('?'),
            CwlType::Array(arr) => arr.iter().any(|t| t == "null"),
        }
    }

    pub fn is_array(&self) -> bool {
        match self {
            CwlType::Single(s) => s.ends_with("[]"),
            CwlType::Array(_) => false,
        }
    }
}

/// SecondaryFile can be a string pattern or a structured entry.
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

/// A CWL Workflow definition.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
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
    pub requirements: HashMap<String, serde_yaml::Value>,
}

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
    pub output_source: String,
    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowStep {
    pub run: String,
    #[serde(rename = "in")]
    pub inputs: HashMap<String, StepInput>,
    pub out: Vec<String>,
    #[serde(default)]
    pub scatter: Option<ScatterField>,
    #[serde(rename = "scatterMethod")]
    #[serde(default)]
    pub scatter_method: Option<String>,
}

/// A step input can be a simple source string or a structured entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepInput {
    /// Simple: `input_name: source_name` or `input_name: step/output`
    Source(String),
    /// Structured: `{ source: ..., valueFrom: ..., default: ... }`
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

/// scatter can be a single input name or an array of input names.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ScatterField {
    Single(String),
    Multiple(Vec<String>),
}

// --- Runtime types used during execution ---

/// Resolved input values for a workflow or tool run.
#[derive(Debug, Clone)]
pub enum ResolvedValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    File(FileValue),
    Directory(PathBuf),
    Array(Vec<ResolvedValue>),
    Null,
}

/// A resolved File value with path and metadata.
#[derive(Debug, Clone)]
pub struct FileValue {
    pub path: PathBuf,
    pub basename: String,
    pub nameroot: String,
    pub nameext: String,
    pub size: u64,
    pub secondary_files: Vec<FileValue>,
}

impl FileValue {
    pub fn from_path(path: PathBuf) -> Self {
        let basename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let nameext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let nameroot = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, basename, nameroot, nameext, size, secondary_files: vec![] }
    }
}

/// Runtime context available during parameter reference resolution.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    pub cores: u32,
    pub ram: u64,
    pub outdir: PathBuf,
    pub tmpdir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwl_type_base_type() {
        assert_eq!(CwlType::Single("File".into()).base_type(), "File");
        assert_eq!(CwlType::Single("File?".into()).base_type(), "File");
        assert_eq!(CwlType::Single("string[]".into()).base_type(), "string");
        assert_eq!(CwlType::Array(vec!["null".into(), "File".into()]).base_type(), "File");
    }

    #[test]
    fn cwl_type_optional() {
        assert!(!CwlType::Single("File".into()).is_optional());
        assert!(CwlType::Single("File?".into()).is_optional());
        assert!(CwlType::Array(vec!["null".into(), "File".into()]).is_optional());
    }

    #[test]
    fn cwl_type_array() {
        assert!(!CwlType::Single("File".into()).is_array());
        assert!(CwlType::Single("File[]".into()).is_array());
    }

    #[test]
    fn file_value_from_path() {
        let fv = FileValue::from_path(PathBuf::from("/tmp/sample.sorted.bam"));
        assert_eq!(fv.basename, "sample.sorted.bam");
        assert_eq!(fv.nameext, ".bam");
        assert_eq!(fv.nameroot, "sample.sorted");
    }
}
```

- [ ] **Step 4: Write minimal lib.rs and main.rs**

`src/lib.rs`:
```rust
pub mod model;
```

`src/main.rs`:
```rust
fn main() {
    println!("cwl-zen: not yet implemented");
}
```

- [ ] **Step 5: Run tests and verify**

```bash
cd ~/repos/cwl-zen && cargo test
```

Expected: all model tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/
git commit -m "feat: project scaffold and CWL data model"
```

---

## Task 2: CWL Document Parser

**Files:**
- Create: `src/parse.rs`
- Create: `tests/fixtures/echo.cwl`
- Create: `tests/fixtures/cat.cwl`
- Create: `tests/fixtures/add-prefix.cwl`
- Create: `tests/fixtures/two-step.cwl`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create test fixtures**

`tests/fixtures/echo.cwl`:
```yaml
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo

inputs:
  message:
    type: string
    inputBinding:
      position: 1

outputs:
  output:
    type: stdout

stdout: output.txt
```

`tests/fixtures/cat.cwl`:
```yaml
cwlVersion: v1.2
class: CommandLineTool
baseCommand: cat

inputs:
  input_file:
    type: File
    inputBinding:
      position: 1

stdout: output.txt

outputs:
  output:
    type: File
    outputBinding:
      glob: output.txt
```

`tests/fixtures/add-prefix.cwl`:
```yaml
cwlVersion: v1.2
class: CommandLineTool
baseCommand: []

requirements:
  ShellCommandRequirement: {}

inputs:
  prefix:
    type: string
  input_file:
    type: File

arguments:
  - shellQuote: false
    valueFrom: |
      echo "$(inputs.prefix)_\$(basename $(inputs.input_file.path))"

stdout: output.txt

outputs:
  output:
    type: File
    outputBinding:
      glob: output.txt
```

`tests/fixtures/two-step.cwl`:
```yaml
cwlVersion: v1.2
class: Workflow

inputs:
  message:
    type: string
  prefix:
    type: string

steps:
  echo_step:
    run: echo.cwl
    in:
      message: message
    out: [output]
  cat_step:
    run: cat.cwl
    in:
      input_file: echo_step/output
    out: [output]

outputs:
  final_output:
    type: File
    outputSource: cat_step/output
```

- [ ] **Step 2: Write the parser with tests**

`src/parse.rs`:
```rust
use crate::model::*;
use anyhow::{Context, Result};
use std::path::Path;

/// Parse a CWL document from a YAML file.
pub fn parse_cwl(path: &Path) -> Result<CwlDocument> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read CWL file: {}", path.display()))?;
    parse_cwl_str(&content)
}

/// Parse a CWL document from a YAML string.
pub fn parse_cwl_str(content: &str) -> Result<CwlDocument> {
    // Strip shebang line if present
    let content = if content.starts_with("#!") {
        content.splitn(2, '\n').nth(1).unwrap_or("")
    } else {
        content
    };
    let doc: CwlDocument = serde_yaml::from_str(content)
        .context("Failed to parse CWL document")?;
    Ok(doc)
}

/// Extract DockerRequirement image from a tool's requirements or hints.
pub fn docker_image(tool: &CommandLineTool) -> Option<String> {
    for section in [&tool.requirements, &tool.hints] {
        if let Some(val) = section.get("DockerRequirement") {
            if let Some(image) = val.get("dockerPull").and_then(|v| v.as_str()) {
                return Some(image.to_string());
            }
        }
    }
    None
}

/// Check if ShellCommandRequirement is present.
pub fn has_shell_requirement(tool: &CommandLineTool) -> bool {
    tool.requirements.contains_key("ShellCommandRequirement")
}

/// Extract ResourceRequirement values.
pub fn resource_requirement(tool: &CommandLineTool) -> (u32, u64) {
    let default_cores = 1u32;
    let default_ram = 1024u64; // MB
    let req = match tool.requirements.get("ResourceRequirement") {
        Some(v) => v,
        None => return (default_cores, default_ram),
    };
    let cores = req.get("coresMin")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(default_cores);
    let ram = req.get("ramMin")
        .and_then(|v| v.as_u64())
        .unwrap_or(default_ram);
    (cores, ram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_echo_tool() {
        let doc = parse_cwl(Path::new("tests/fixtures/echo.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(matches!(tool.base_command, BaseCommand::Single(ref s) if s == "echo"));
                assert!(tool.inputs.contains_key("message"));
                assert_eq!(tool.inputs["message"].cwl_type.base_type(), "string");
                assert_eq!(tool.inputs["message"].input_binding.as_ref().unwrap().position, Some(1));
                assert_eq!(tool.stdout.as_deref(), Some("output.txt"));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_cat_tool() {
        let doc = parse_cwl(Path::new("tests/fixtures/cat.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(tool.inputs.contains_key("input_file"));
                assert_eq!(tool.inputs["input_file"].cwl_type.base_type(), "File");
                let glob = &tool.outputs["output"].output_binding.as_ref().unwrap().glob;
                assert!(matches!(glob, GlobPattern::Single(ref s) if s == "output.txt"));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_add_prefix_tool() {
        let doc = parse_cwl(Path::new("tests/fixtures/add-prefix.cwl")).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(has_shell_requirement(&tool));
                assert!(matches!(tool.base_command, BaseCommand::Array(ref a) if a.is_empty()));
                assert_eq!(tool.arguments.len(), 1);
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_two_step_workflow() {
        let doc = parse_cwl(Path::new("tests/fixtures/two-step.cwl")).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                assert_eq!(wf.steps.len(), 2);
                assert!(wf.steps.contains_key("echo_step"));
                assert!(wf.steps.contains_key("cat_step"));
                assert_eq!(wf.steps["echo_step"].run, "echo.cwl");
                assert_eq!(wf.steps["echo_step"].out, vec!["output"]);
                // cat_step input is echo_step/output
                let cat_in = &wf.steps["cat_step"].inputs["input_file"];
                assert!(matches!(cat_in, StepInput::Source(ref s) if s == "echo_step/output"));
                // workflow output
                assert_eq!(wf.outputs["final_output"].output_source, "cat_step/output");
            }
            _ => panic!("Expected Workflow"),
        }
    }

    #[test]
    fn parse_docker_image() {
        let doc = parse_cwl_str(r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: bwa
hints:
  DockerRequirement:
    dockerPull: "quay.io/biocontainers/bwa-mem2:2.2.1"
inputs: {}
outputs: {}
"#).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert_eq!(docker_image(&tool).as_deref(), Some("quay.io/biocontainers/bwa-mem2:2.2.1"));
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_resource_requirement() {
        let doc = parse_cwl_str(r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
requirements:
  ResourceRequirement:
    coresMin: 8
    ramMin: 16384
inputs: {}
outputs: {}
"#).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let (cores, ram) = resource_requirement(&tool);
                assert_eq!(cores, 8);
                assert_eq!(ram, 16384);
            }
            _ => panic!("Expected CommandLineTool"),
        }
    }
}
```

- [ ] **Step 3: Update lib.rs**

```rust
pub mod model;
pub mod parse;
```

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

Expected: all parse tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/parse.rs src/lib.rs tests/fixtures/
git commit -m "feat: CWL document parser with tool and workflow support"
```

---

## Task 3: Parameter Reference Resolver

**Files:**
- Create: `src/param.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write tests first**

The parameter reference resolver handles `$(inputs.X)`, `$(inputs.X.path)`, `$(runtime.cores)`, `$(self)` and string interpolation like `$(inputs.sample_id).bam`.

- [ ] **Step 2: Implement param.rs with tests**

`src/param.rs`:
```rust
use crate::model::{ResolvedValue, RuntimeContext};
use std::collections::HashMap;

/// Resolve all `$(...)` parameter references in a string.
///
/// CWL parameter references are `$(inputs.X)`, `$(inputs.X.path)`,
/// `$(runtime.cores)`, `$(self)`, etc. This function handles string
/// interpolation: `$(inputs.sample_id).bam` becomes `SRX123.bam`.
///
/// Escaped `\$(...)` is left as `$(...)` for shell command substitution.
pub fn resolve_param_refs(
    template: &str,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
    self_val: Option<&ResolvedValue>,
) -> String {
    let mut result = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Check for escaped \$( — pass through as $(
        if i + 2 < len && chars[i] == '\\' && chars[i + 1] == '$' {
            result.push('$');
            i += 2;
            continue;
        }
        // Check for $( — parameter reference
        if i + 1 < len && chars[i] == '$' && chars[i + 1] == '(' {
            // Find matching closing )
            let start = i + 2;
            let mut depth = 1;
            let mut end = start;
            while end < len && depth > 0 {
                if chars[end] == '(' { depth += 1; }
                if chars[end] == ')' { depth -= 1; }
                if depth > 0 { end += 1; }
            }
            if depth == 0 {
                let expr: String = chars[start..end].iter().collect();
                let resolved = resolve_expression(&expr, inputs, runtime, self_val);
                result.push_str(&resolved);
                i = end + 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

/// Resolve a single parameter reference expression (the part inside `$(...)`).
fn resolve_expression(
    expr: &str,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
    self_val: Option<&ResolvedValue>,
) -> String {
    let expr = expr.trim();

    // $(self) or $(self.property)
    if expr == "self" {
        return value_to_string(self_val.unwrap_or(&ResolvedValue::Null));
    }
    if let Some(prop) = expr.strip_prefix("self.") {
        return match self_val {
            Some(val) => resolve_property(val, prop),
            None => "null".to_string(),
        };
    }

    // $(runtime.X)
    if let Some(prop) = expr.strip_prefix("runtime.") {
        return match prop {
            "cores" => runtime.cores.to_string(),
            "ram" => runtime.ram.to_string(),
            "outdir" => runtime.outdir.to_string_lossy().to_string(),
            "tmpdir" => runtime.tmpdir.to_string_lossy().to_string(),
            _ => format!("$(runtime.{})", prop),
        };
    }

    // $(inputs.X) or $(inputs.X.property)
    if let Some(rest) = expr.strip_prefix("inputs.") {
        let (name, property) = match rest.find('.') {
            Some(dot) => (&rest[..dot], Some(&rest[dot + 1..])),
            None => (rest, None),
        };
        return match inputs.get(name) {
            Some(val) => match property {
                Some(prop) => resolve_property(val, prop),
                None => value_to_string(val),
            },
            None => "null".to_string(),
        };
    }

    // Unrecognized — return as-is
    format!("$({})", expr)
}

/// Resolve a property access on a ResolvedValue (e.g., .path, .basename).
fn resolve_property(val: &ResolvedValue, prop: &str) -> String {
    match val {
        ResolvedValue::File(f) => match prop {
            "path" => f.path.to_string_lossy().to_string(),
            "basename" => f.basename.clone(),
            "nameroot" => f.nameroot.clone(),
            "nameext" => f.nameext.clone(),
            "size" => f.size.to_string(),
            _ => "null".to_string(),
        },
        _ => value_to_string(val),
    }
}

/// Convert a ResolvedValue to its string representation.
pub fn value_to_string(val: &ResolvedValue) -> String {
    match val {
        ResolvedValue::String(s) => s.clone(),
        ResolvedValue::Int(n) => n.to_string(),
        ResolvedValue::Float(f) => f.to_string(),
        ResolvedValue::Bool(b) => b.to_string(),
        ResolvedValue::File(f) => f.path.to_string_lossy().to_string(),
        ResolvedValue::Directory(p) => p.to_string_lossy().to_string(),
        ResolvedValue::Null => "null".to_string(),
        ResolvedValue::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(value_to_string).collect();
            parts.join(" ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileValue;
    use std::path::PathBuf;

    fn test_inputs() -> HashMap<String, ResolvedValue> {
        let mut inputs = HashMap::new();
        inputs.insert("sample_id".to_string(), ResolvedValue::String("SRX123".to_string()));
        inputs.insert("count".to_string(), ResolvedValue::Int(42));
        inputs.insert("bam".to_string(), ResolvedValue::File(FileValue {
            path: PathBuf::from("/data/sample.sorted.bam"),
            basename: "sample.sorted.bam".to_string(),
            nameroot: "sample.sorted".to_string(),
            nameext: ".bam".to_string(),
            size: 1024,
            secondary_files: vec![],
        }));
        inputs
    }

    fn test_runtime() -> RuntimeContext {
        RuntimeContext {
            cores: 8,
            ram: 16384,
            outdir: PathBuf::from("/output"),
            tmpdir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn resolve_simple_input() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let result = resolve_param_refs("$(inputs.sample_id)", &inputs, &rt, None);
        assert_eq!(result, "SRX123");
    }

    #[test]
    fn resolve_string_interpolation() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let result = resolve_param_refs("$(inputs.sample_id).sorted.bam", &inputs, &rt, None);
        assert_eq!(result, "SRX123.sorted.bam");
    }

    #[test]
    fn resolve_file_properties() {
        let inputs = test_inputs();
        let rt = test_runtime();
        assert_eq!(resolve_param_refs("$(inputs.bam.path)", &inputs, &rt, None), "/data/sample.sorted.bam");
        assert_eq!(resolve_param_refs("$(inputs.bam.basename)", &inputs, &rt, None), "sample.sorted.bam");
        assert_eq!(resolve_param_refs("$(inputs.bam.nameroot)", &inputs, &rt, None), "sample.sorted");
        assert_eq!(resolve_param_refs("$(inputs.bam.nameext)", &inputs, &rt, None), ".bam");
        assert_eq!(resolve_param_refs("$(inputs.bam.size)", &inputs, &rt, None), "1024");
    }

    #[test]
    fn resolve_runtime() {
        let inputs = test_inputs();
        let rt = test_runtime();
        assert_eq!(resolve_param_refs("$(runtime.cores)", &inputs, &rt, None), "8");
        assert_eq!(resolve_param_refs("$(runtime.ram)", &inputs, &rt, None), "16384");
    }

    #[test]
    fn resolve_self() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let self_val = ResolvedValue::String("SRX123".to_string());
        let result = resolve_param_refs("$(self).05", &inputs, &rt, Some(&self_val));
        assert_eq!(result, "SRX123.05");
    }

    #[test]
    fn resolve_escaped_dollar() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let result = resolve_param_refs(r"\$(cat $(inputs.bam.path))", &inputs, &rt, None);
        assert_eq!(result, "$(cat /data/sample.sorted.bam)");
    }

    #[test]
    fn resolve_mixed_cwl_and_shell() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let template = r"COUNT=\$(cat $(inputs.bam.path))
tool -c \$COUNT -t $(runtime.cores)";
        let result = resolve_param_refs(template, &inputs, &rt, None);
        assert_eq!(result, "COUNT=$(cat /data/sample.sorted.bam)\ntool -c $COUNT -t 8");
    }

    #[test]
    fn resolve_rg_header() {
        let inputs = test_inputs();
        let rt = test_runtime();
        let template = r"@RG\tID:$(inputs.sample_id)\tSM:$(inputs.sample_id)";
        let result = resolve_param_refs(template, &inputs, &rt, None);
        assert_eq!(result, r"@RG\tID:SRX123\tSM:SRX123");
    }

    #[test]
    fn resolve_null_input() {
        let inputs = test_inputs();
        let rt = test_runtime();
        assert_eq!(resolve_param_refs("$(inputs.missing)", &inputs, &rt, None), "null");
    }

    #[test]
    fn resolve_int_input() {
        let inputs = test_inputs();
        let rt = test_runtime();
        assert_eq!(resolve_param_refs("$(inputs.count)", &inputs, &rt, None), "42");
    }
}
```

- [ ] **Step 3: Update lib.rs**

Add `pub mod param;`

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test param
```

Expected: all param tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/param.rs src/lib.rs
git commit -m "feat: parameter reference resolver with escaping support"
```

---

## Task 4: Input YAML Parser

**Files:**
- Create: `src/input.rs`
- Create: `tests/fixtures/echo-input.yml`
- Create: `tests/fixtures/cat-input.yml`
- Create: `tests/fixtures/two-step-input.yml`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create input fixtures**

`tests/fixtures/echo-input.yml`:
```yaml
message: "Hello, CWL Zen!"
```

`tests/fixtures/cat-input.yml`:
```yaml
input_file:
  class: File
  path: tests/fixtures/echo-input.yml
```

`tests/fixtures/two-step-input.yml`:
```yaml
message: "Hello from two-step"
prefix: "ZEN"
```

- [ ] **Step 2: Implement input.rs with tests**

`src/input.rs`:
```rust
use crate::model::{FileValue, ResolvedValue};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parse an input YAML file and resolve all values relative to a base directory.
pub fn parse_inputs(path: &Path, base_dir: &Path) -> Result<HashMap<String, ResolvedValue>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read input file: {}", path.display()))?;
    let raw: HashMap<String, serde_yaml::Value> = serde_yaml::from_str(&content)
        .context("Failed to parse input YAML")?;

    let mut resolved = HashMap::new();
    for (key, val) in raw {
        resolved.insert(key, resolve_yaml_value(&val, base_dir)?);
    }
    Ok(resolved)
}

/// Convert a serde_yaml::Value into a ResolvedValue.
fn resolve_yaml_value(val: &serde_yaml::Value, base_dir: &Path) -> Result<ResolvedValue> {
    match val {
        serde_yaml::Value::Null => Ok(ResolvedValue::Null),
        serde_yaml::Value::Bool(b) => Ok(ResolvedValue::Bool(*b)),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ResolvedValue::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(ResolvedValue::Float(f))
            } else {
                Ok(ResolvedValue::String(n.to_string()))
            }
        }
        serde_yaml::Value::String(s) => Ok(ResolvedValue::String(s.clone())),
        serde_yaml::Value::Sequence(arr) => {
            let items: Result<Vec<_>> = arr.iter().map(|v| resolve_yaml_value(v, base_dir)).collect();
            Ok(ResolvedValue::Array(items?))
        }
        serde_yaml::Value::Mapping(map) => {
            // Check if this is a File or Directory object
            if let Some(class) = map.get(&serde_yaml::Value::String("class".into())) {
                let class_str = class.as_str().unwrap_or("");
                if class_str == "File" {
                    return resolve_file_value(map, base_dir);
                }
                if class_str == "Directory" {
                    if let Some(path_val) = map.get(&serde_yaml::Value::String("path".into())) {
                        let p = path_val.as_str().unwrap_or("");
                        let abs = if Path::new(p).is_absolute() {
                            PathBuf::from(p)
                        } else {
                            base_dir.join(p)
                        };
                        return Ok(ResolvedValue::Directory(abs));
                    }
                }
            }
            // Generic mapping — treat as string representation
            Ok(ResolvedValue::String(serde_yaml::to_string(map)?))
        }
        _ => Ok(ResolvedValue::Null),
    }
}

/// Resolve a CWL File object from input YAML.
fn resolve_file_value(
    map: &serde_yaml::Mapping,
    base_dir: &Path,
) -> Result<ResolvedValue> {
    let path_str = map
        .get(&serde_yaml::Value::String("path".into()))
        .or_else(|| map.get(&serde_yaml::Value::String("location".into())))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let abs_path = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        base_dir.join(path_str)
    };

    let mut file_val = FileValue::from_path(abs_path);

    // Resolve secondaryFiles if present
    if let Some(serde_yaml::Value::Sequence(sec_files)) =
        map.get(&serde_yaml::Value::String("secondaryFiles".into()))
    {
        for sf in sec_files {
            if let serde_yaml::Value::Mapping(sf_map) = sf {
                if let Ok(ResolvedValue::File(sf_val)) = resolve_file_value(sf_map, base_dir) {
                    file_val.secondary_files.push(sf_val);
                }
            }
        }
    }

    Ok(ResolvedValue::File(file_val))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_echo_input() {
        let inputs = parse_inputs(
            Path::new("tests/fixtures/echo-input.yml"),
            Path::new("tests/fixtures"),
        ).unwrap();
        assert!(matches!(inputs.get("message"), Some(ResolvedValue::String(s)) if s == "Hello, CWL Zen!"));
    }

    #[test]
    fn parse_cat_input() {
        let inputs = parse_inputs(
            Path::new("tests/fixtures/cat-input.yml"),
            Path::new("tests/fixtures"),
        ).unwrap();
        match inputs.get("input_file") {
            Some(ResolvedValue::File(f)) => {
                assert!(f.path.to_string_lossy().contains("echo-input.yml"));
            }
            other => panic!("Expected File, got {:?}", other),
        }
    }

    #[test]
    fn parse_two_step_input() {
        let inputs = parse_inputs(
            Path::new("tests/fixtures/two-step-input.yml"),
            Path::new("tests/fixtures"),
        ).unwrap();
        assert!(matches!(inputs.get("message"), Some(ResolvedValue::String(s)) if s == "Hello from two-step"));
        assert!(matches!(inputs.get("prefix"), Some(ResolvedValue::String(s)) if s == "ZEN"));
    }
}
```

- [ ] **Step 3: Update lib.rs**

Add `pub mod input;`

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test input
```

Expected: all input tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/input.rs src/lib.rs tests/fixtures/*.yml
git commit -m "feat: input YAML parser with File and secondary file resolution"
```

---

## Task 5: DAG Builder

**Files:**
- Create: `src/dag.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Implement DAG builder with tests**

`src/dag.rs`:
```rust
use crate::model::{StepInput, Workflow};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet, VecDeque};

/// A step in the execution DAG with its dependencies resolved.
#[derive(Debug, Clone)]
pub struct DagStep {
    pub name: String,
    pub tool_path: String,
    pub depends_on: Vec<String>,
}

/// Build a DAG from a workflow definition.
/// Returns steps in topological order (dependencies first).
pub fn build_dag(workflow: &Workflow) -> Result<Vec<DagStep>> {
    let step_names: HashSet<&str> = workflow.steps.keys().map(|s| s.as_str()).collect();
    let mut dag_steps = Vec::new();

    for (name, step) in &workflow.steps {
        let mut depends_on = Vec::new();
        for (_input_name, input_val) in &step.inputs {
            let source = match input_val {
                StepInput::Source(s) => Some(s.as_str()),
                StepInput::Structured(entry) => entry.source.as_deref(),
            };
            if let Some(src) = source {
                // Source format: "step_name/output_name" means dependency on step_name.
                // Source without "/" is a workflow-level input — no dependency.
                if let Some(dep_step) = src.split('/').next() {
                    if step_names.contains(dep_step) && !depends_on.contains(&dep_step.to_string()) {
                        depends_on.push(dep_step.to_string());
                    }
                }
            }
        }
        dag_steps.push(DagStep {
            name: name.clone(),
            tool_path: step.run.clone(),
            depends_on,
        });
    }

    topological_sort(dag_steps)
}

/// Topological sort using Kahn's algorithm.
fn topological_sort(steps: Vec<DagStep>) -> Result<Vec<DagStep>> {
    let step_map: HashMap<&str, &DagStep> = steps.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for step in &steps {
        in_degree.entry(step.name.as_str()).or_insert(0);
        for dep in &step.depends_on {
            *in_degree.entry(step.name.as_str()).or_insert(0) += 1;
            dependents.entry(dep.as_str()).or_default().push(step.name.as_str());
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    let mut sorted = Vec::new();
    while let Some(name) = queue.pop_front() {
        sorted.push(name);
        if let Some(deps) = dependents.get(name) {
            for &dep in deps {
                let deg = in_degree.get_mut(dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    if sorted.len() != steps.len() {
        bail!("Cyclic dependency detected in workflow steps");
    }

    Ok(sorted
        .into_iter()
        .map(|name| step_map[name].clone())
        .collect())
}

/// Print the DAG as a human-readable dependency graph.
pub fn print_dag(steps: &[DagStep]) {
    for (i, step) in steps.iter().enumerate() {
        let deps = if step.depends_on.is_empty() {
            "(no dependencies)".to_string()
        } else {
            format!("depends on: {}", step.depends_on.join(", "))
        };
        println!("  {}. {} [{}] {}", i + 1, step.name, step.tool_path, deps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_cwl;
    use crate::model::CwlDocument;
    use std::path::Path;

    #[test]
    fn dag_two_step_workflow() {
        let doc = parse_cwl(Path::new("tests/fixtures/two-step.cwl")).unwrap();
        match doc {
            CwlDocument::Workflow(wf) => {
                let dag = build_dag(&wf).unwrap();
                assert_eq!(dag.len(), 2);
                // echo_step has no dependencies, so it comes first
                assert_eq!(dag[0].name, "echo_step");
                assert!(dag[0].depends_on.is_empty());
                // cat_step depends on echo_step
                assert_eq!(dag[1].name, "cat_step");
                assert_eq!(dag[1].depends_on, vec!["echo_step"]);
            }
            _ => panic!("Expected Workflow"),
        }
    }

    #[test]
    fn dag_detects_cycle() {
        // Manually build a cyclic workflow
        let steps = vec![
            DagStep { name: "a".into(), tool_path: "a.cwl".into(), depends_on: vec!["b".into()] },
            DagStep { name: "b".into(), tool_path: "b.cwl".into(), depends_on: vec!["a".into()] },
        ];
        assert!(topological_sort(steps).is_err());
    }

    #[test]
    fn dag_parallel_steps() {
        // Two independent steps
        let steps = vec![
            DagStep { name: "a".into(), tool_path: "a.cwl".into(), depends_on: vec![] },
            DagStep { name: "b".into(), tool_path: "b.cwl".into(), depends_on: vec![] },
        ];
        let sorted = topological_sort(steps).unwrap();
        assert_eq!(sorted.len(), 2);
    }

    #[test]
    fn dag_diamond() {
        // A → B, A → C, B → D, C → D
        let steps = vec![
            DagStep { name: "a".into(), tool_path: "a.cwl".into(), depends_on: vec![] },
            DagStep { name: "b".into(), tool_path: "b.cwl".into(), depends_on: vec!["a".into()] },
            DagStep { name: "c".into(), tool_path: "c.cwl".into(), depends_on: vec!["a".into()] },
            DagStep { name: "d".into(), tool_path: "d.cwl".into(), depends_on: vec!["b".into(), "c".into()] },
        ];
        let sorted = topological_sort(steps).unwrap();
        assert_eq!(sorted[0].name, "a");
        assert_eq!(sorted[3].name, "d");
        // b and c can be in either order
        let mid: HashSet<&str> = sorted[1..3].iter().map(|s| s.name.as_str()).collect();
        assert!(mid.contains("b") && mid.contains("c"));
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add `pub mod dag;`

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test dag
```

Expected: all dag tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/dag.rs src/lib.rs
git commit -m "feat: DAG builder with topological sort and cycle detection"
```

---

## Task 6: Command Builder

**Files:**
- Create: `src/command.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Implement command builder with tests**

This module takes a parsed CommandLineTool + resolved inputs + runtime context, and produces the shell command string and Docker run arguments.

`src/command.rs`:
```rust
use crate::model::*;
use crate::param::resolve_param_refs;
use std::collections::HashMap;
use std::path::Path;

/// A fully resolved command ready for execution.
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    /// The complete command string to execute.
    pub command_line: String,
    /// Whether to run via `sh -c` (ShellCommandRequirement).
    pub use_shell: bool,
    /// Docker image to use, if any.
    pub docker_image: Option<String>,
    /// Requested CPU cores.
    pub cores: u32,
    /// Requested RAM in MB.
    pub ram: u64,
    /// Stdout redirect filename, if any.
    pub stdout_file: Option<String>,
}

/// Build a resolved command from a tool definition and inputs.
pub fn build_command(
    tool: &CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> ResolvedCommand {
    let use_shell = tool.requirements.contains_key("ShellCommandRequirement");
    let docker_image = crate::parse::docker_image(tool);
    let (cores, ram) = crate::parse::resource_requirement(tool);

    // Override runtime with tool's resource requirements
    let runtime = RuntimeContext {
        cores,
        ram,
        ..runtime.clone()
    };

    // Build base command parts
    let mut parts: Vec<(i32, String)> = Vec::new();

    match &tool.base_command {
        BaseCommand::Single(s) => parts.push((-1000, s.clone())),
        BaseCommand::Array(arr) => {
            for s in arr {
                parts.push((-1000, s.clone()));
            }
        }
        BaseCommand::None => {}
    }

    // Add arguments
    for arg in &tool.arguments {
        match arg {
            Argument::String(s) => {
                let resolved = resolve_param_refs(s, inputs, &runtime, None);
                parts.push((0, resolved));
            }
            Argument::Structured(entry) => {
                let pos = entry.position.unwrap_or(0);
                let val = entry.value_from.as_deref().unwrap_or("");
                let resolved = resolve_param_refs(val, inputs, &runtime, None);
                if let Some(prefix) = &entry.prefix {
                    parts.push((pos, format!("{} {}", prefix, resolved)));
                } else {
                    parts.push((pos, resolved));
                }
            }
        }
    }

    // Add inputs with inputBinding
    let mut bound_inputs: Vec<(i32, String)> = Vec::new();
    for (name, input_def) in &tool.inputs {
        if let Some(binding) = &input_def.input_binding {
            let pos = binding.position.unwrap_or(0);
            let val = match inputs.get(name) {
                Some(v) => {
                    if let Some(vf) = &binding.value_from {
                        resolve_param_refs(vf, inputs, &runtime, Some(v))
                    } else {
                        crate::param::value_to_string(v)
                    }
                }
                None => {
                    if let Some(def) = &input_def.default {
                        def.as_str().unwrap_or("").to_string()
                    } else if input_def.cwl_type.is_optional() {
                        continue; // Skip optional inputs with no value
                    } else {
                        continue;
                    }
                }
            };
            if let Some(prefix) = &binding.prefix {
                bound_inputs.push((pos, format!("{} {}", prefix, val)));
            } else {
                bound_inputs.push((pos, val));
            }
        }
    }
    parts.extend(bound_inputs);

    // Sort by position (stable sort preserves order within same position)
    parts.sort_by_key(|(pos, _)| *pos);

    let command_line = parts.into_iter().map(|(_, s)| s).collect::<Vec<_>>().join(" ");

    let stdout_file = tool.stdout.as_ref().map(|s| {
        resolve_param_refs(s, inputs, &runtime, None)
    });

    ResolvedCommand {
        command_line,
        use_shell,
        docker_image,
        cores,
        ram,
        stdout_file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_cwl;
    use std::path::PathBuf;

    fn basic_runtime() -> RuntimeContext {
        RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: PathBuf::from("/output"),
            tmpdir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn build_echo_command() {
        let doc = crate::parse::parse_cwl(Path::new("tests/fixtures/echo.cwl")).unwrap();
        let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
        let mut inputs = HashMap::new();
        inputs.insert("message".into(), ResolvedValue::String("hello".into()));
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.command_line, "echo hello");
        assert!(!cmd.use_shell);
        assert!(cmd.docker_image.is_none());
        assert_eq!(cmd.stdout_file.as_deref(), Some("output.txt"));
    }

    #[test]
    fn build_cat_command() {
        let doc = crate::parse::parse_cwl(Path::new("tests/fixtures/cat.cwl")).unwrap();
        let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
        let mut inputs = HashMap::new();
        inputs.insert("input_file".into(), ResolvedValue::File(FileValue::from_path(PathBuf::from("/data/test.txt"))));
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.command_line, "cat /data/test.txt");
    }

    #[test]
    fn build_shell_command() {
        let doc = crate::parse::parse_cwl(Path::new("tests/fixtures/add-prefix.cwl")).unwrap();
        let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
        let mut inputs = HashMap::new();
        inputs.insert("prefix".into(), ResolvedValue::String("ZEN".into()));
        inputs.insert("input_file".into(), ResolvedValue::File(FileValue::from_path(PathBuf::from("/data/test.txt"))));
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert!(cmd.use_shell);
        assert!(cmd.command_line.contains("ZEN"));
        assert!(cmd.command_line.contains("/data/test.txt"));
    }

    #[test]
    fn build_docker_command() {
        let doc = crate::parse::parse_cwl_str(r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: bwa
hints:
  DockerRequirement:
    dockerPull: "quay.io/biocontainers/bwa:0.7.17"
requirements:
  ResourceRequirement:
    coresMin: 8
    ramMin: 16384
inputs:
  ref:
    type: File
    inputBinding:
      position: 1
outputs: {}
"#).unwrap();
        let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
        let mut inputs = HashMap::new();
        inputs.insert("ref".into(), ResolvedValue::File(FileValue::from_path(PathBuf::from("/data/ref.fa"))));
        let cmd = build_command(&tool, &inputs, &basic_runtime());
        assert_eq!(cmd.docker_image.as_deref(), Some("quay.io/biocontainers/bwa:0.7.17"));
        assert_eq!(cmd.cores, 8);
        assert_eq!(cmd.ram, 16384);
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add `pub mod command;`

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test command
```

Expected: all command tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/command.rs src/lib.rs
git commit -m "feat: command builder from CWL tool definitions"
```

---

## Task 7: Stage and Output Collection

**Files:**
- Create: `src/stage.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Implement staging and glob with tests**

`src/stage.rs`:
```rust
use crate::model::*;
use crate::param::resolve_param_refs;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Collect output files from a working directory using glob patterns.
pub fn collect_outputs(
    tool: &CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
    workdir: &Path,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut outputs = HashMap::new();

    for (name, output_def) in &tool.outputs {
        if let Some(binding) = &output_def.output_binding {
            let patterns = resolve_glob_patterns(&binding.glob, inputs, runtime);
            let mut matched_files = Vec::new();

            for pattern in &patterns {
                let full_pattern = workdir.join(pattern);
                let pattern_str = full_pattern.to_string_lossy().to_string();
                for entry in glob::glob(&pattern_str).context("Invalid glob pattern")? {
                    if let Ok(path) = entry {
                        let mut fv = FileValue::from_path(path.clone());
                        // Collect secondary files
                        for sf in &output_def.secondary_files {
                            let sf_pattern = match sf {
                                SecondaryFile::Pattern(p) => p.clone(),
                                SecondaryFile::Structured(s) => s.pattern.clone(),
                            };
                            let sf_path = resolve_secondary_file_path(&path, &sf_pattern);
                            if sf_path.exists() {
                                fv.secondary_files.push(FileValue::from_path(sf_path));
                            }
                        }
                        matched_files.push(fv);
                    }
                }
            }

            let value = if output_def.cwl_type.is_array() {
                ResolvedValue::Array(matched_files.into_iter().map(ResolvedValue::File).collect())
            } else if let Some(f) = matched_files.into_iter().next() {
                ResolvedValue::File(f)
            } else if output_def.cwl_type.is_optional() {
                ResolvedValue::Null
            } else {
                ResolvedValue::Null
            };

            outputs.insert(name.clone(), value);
        }
    }

    // Handle stdout output
    if let Some(stdout_name) = &tool.stdout {
        let stdout_file = resolve_param_refs(stdout_name, inputs, runtime, None);
        let stdout_path = workdir.join(&stdout_file);
        if stdout_path.exists() {
            // Find the output that has type stdout
            for (name, output_def) in &tool.outputs {
                if let CwlType::Single(ref t) = output_def.cwl_type {
                    if t == "stdout" {
                        outputs.insert(name.clone(), ResolvedValue::File(FileValue::from_path(stdout_path.clone())));
                    }
                }
            }
        }
    }

    Ok(outputs)
}

/// Resolve glob patterns, expanding parameter references.
fn resolve_glob_patterns(
    glob: &GlobPattern,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Vec<String> {
    match glob {
        GlobPattern::Single(s) => vec![resolve_param_refs(s, inputs, runtime, None)],
        GlobPattern::Array(arr) => arr
            .iter()
            .map(|s| resolve_param_refs(s, inputs, runtime, None))
            .collect(),
        GlobPattern::None => vec![],
    }
}

/// Resolve a secondary file path from a primary file and a pattern.
/// Pattern can be:
/// - A suffix like ".bai" → append to primary path
/// - A caret suffix like "^.bai" → replace extension, then append
pub fn resolve_secondary_file_path(primary: &Path, pattern: &str) -> PathBuf {
    if pattern.starts_with('^') {
        // Replace extension
        let new_ext = &pattern[1..];
        primary.with_extension("").with_extension(new_ext.trim_start_matches('.'))
    } else {
        // Append suffix
        PathBuf::from(format!("{}{}", primary.display(), pattern))
    }
}

/// Build Docker volume mount arguments for input files.
pub fn build_docker_mounts(
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
) -> Vec<String> {
    let mut mounts = Vec::new();
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Mount workdir
    mounts.push(format!("--mount=type=bind,source={},target=/work", workdir.display()));
    seen_dirs.insert(workdir.to_path_buf());

    // Mount each input file's parent directory (read-only)
    fn collect_file_mounts(
        val: &ResolvedValue,
        mounts: &mut Vec<String>,
        seen_dirs: &mut std::collections::HashSet<PathBuf>,
    ) {
        match val {
            ResolvedValue::File(f) => {
                if let Some(parent) = f.path.parent() {
                    let parent = parent.to_path_buf();
                    if seen_dirs.insert(parent.clone()) {
                        mounts.push(format!(
                            "--mount=type=bind,source={},target={},readonly",
                            parent.display(),
                            parent.display()
                        ));
                    }
                }
                for sf in &f.secondary_files {
                    collect_file_mounts(&ResolvedValue::File(sf.clone()), mounts, seen_dirs);
                }
            }
            ResolvedValue::Array(arr) => {
                for v in arr {
                    collect_file_mounts(v, mounts, seen_dirs);
                }
            }
            _ => {}
        }
    }

    for val in inputs.values() {
        collect_file_mounts(val, &mut mounts, &mut seen_dirs);
    }

    mounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_file_suffix() {
        let primary = PathBuf::from("/data/sample.sorted.bam");
        let sf = resolve_secondary_file_path(&primary, ".bai");
        assert_eq!(sf, PathBuf::from("/data/sample.sorted.bam.bai"));
    }

    #[test]
    fn secondary_file_caret() {
        let primary = PathBuf::from("/data/sample.sorted.bam");
        let sf = resolve_secondary_file_path(&primary, "^.bai");
        assert_eq!(sf, PathBuf::from("/data/sample.sorted.bai"));
    }

    #[test]
    fn resolve_glob_single() {
        let inputs = HashMap::new();
        let rt = RuntimeContext {
            cores: 1, ram: 1024,
            outdir: PathBuf::from("/out"),
            tmpdir: PathBuf::from("/tmp"),
        };
        let patterns = resolve_glob_patterns(
            &GlobPattern::Single("*.bam".to_string()),
            &inputs,
            &rt,
        );
        assert_eq!(patterns, vec!["*.bam"]);
    }

    #[test]
    fn resolve_glob_with_param() {
        let mut inputs = HashMap::new();
        inputs.insert("sample_id".into(), ResolvedValue::String("SRX123".into()));
        let rt = RuntimeContext {
            cores: 1, ram: 1024,
            outdir: PathBuf::from("/out"),
            tmpdir: PathBuf::from("/tmp"),
        };
        let patterns = resolve_glob_patterns(
            &GlobPattern::Single("$(inputs.sample_id).sorted.bam".to_string()),
            &inputs,
            &rt,
        );
        assert_eq!(patterns, vec!["SRX123.sorted.bam"]);
    }

    #[test]
    fn resolve_glob_array() {
        let mut inputs = HashMap::new();
        inputs.insert("sample_id".into(), ResolvedValue::String("SRX123".into()));
        let rt = RuntimeContext {
            cores: 1, ram: 1024,
            outdir: PathBuf::from("/out"),
            tmpdir: PathBuf::from("/tmp"),
        };
        let patterns = resolve_glob_patterns(
            &GlobPattern::Array(vec![
                "$(inputs.sample_id).sorted.bam".to_string(),
                "$(inputs.sample_id).namesorted.bam".to_string(),
            ]),
            &inputs,
            &rt,
        );
        assert_eq!(patterns, vec!["SRX123.sorted.bam", "SRX123.namesorted.bam"]);
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add `pub mod stage;`

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test stage
```

Expected: all stage tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/stage.rs src/lib.rs
git commit -m "feat: file staging, glob output collection, Docker mount builder"
```

---

## Task 8: Step Executor

**Files:**
- Create: `src/execute.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Implement executor with tests**

`src/execute.rs`:
```rust
use crate::command::{build_command, ResolvedCommand};
use crate::dag::DagStep;
use crate::model::*;
use crate::parse::parse_cwl;
use crate::stage::{build_docker_mounts, collect_outputs};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Execute a single CommandLineTool step.
pub fn execute_tool(
    tool: &CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    runtime: &RuntimeContext,
    log_dir: &Path,
    step_name: &str,
) -> Result<(i32, HashMap<String, ResolvedValue>)> {
    std::fs::create_dir_all(workdir)?;
    std::fs::create_dir_all(log_dir)?;

    let resolved = build_command(tool, inputs, runtime);
    let stdout_path = log_dir.join(format!("{}.stdout.log", step_name));
    let stderr_path = log_dir.join(format!("{}.stderr.log", step_name));

    let exit_code = run_command(&resolved, workdir, inputs, &stdout_path, &stderr_path)?;

    // If stdout was redirected, move it to workdir
    if let Some(ref stdout_file) = resolved.stdout_file {
        let dest = workdir.join(stdout_file);
        if stdout_path.exists() && !dest.exists() {
            std::fs::copy(&stdout_path, &dest)?;
        }
    }

    let outputs = collect_outputs(tool, inputs, runtime, workdir)?;

    Ok((exit_code, outputs))
}

/// Run a resolved command, capturing stdout/stderr.
fn run_command(
    cmd: &ResolvedCommand,
    workdir: &Path,
    inputs: &HashMap<String, ResolvedValue>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<i32> {
    let stdout_file = std::fs::File::create(stdout_path)?;
    let stderr_file = std::fs::File::create(stderr_path)?;

    let status = if let Some(ref image) = cmd.docker_image {
        // Run via Docker
        let mounts = build_docker_mounts(inputs, workdir);
        let mut docker_cmd = Command::new("docker");
        docker_cmd
            .arg("run")
            .arg("--rm")
            .arg(format!("--workdir=/work"));

        for mount in &mounts {
            docker_cmd.arg(mount);
        }

        docker_cmd.arg(image);

        if cmd.use_shell {
            docker_cmd.args(["sh", "-c", &cmd.command_line]);
        } else {
            for part in cmd.command_line.split_whitespace() {
                docker_cmd.arg(part);
            }
        }

        docker_cmd
            .stdout(stdout_file)
            .stderr(stderr_file)
            .status()
            .context("Failed to run Docker command")?
    } else {
        // Run directly
        let mut proc = if cmd.use_shell {
            let mut p = Command::new("sh");
            p.args(["-c", &cmd.command_line]);
            p
        } else {
            let parts: Vec<&str> = cmd.command_line.split_whitespace().collect();
            let mut p = Command::new(parts[0]);
            if parts.len() > 1 {
                p.args(&parts[1..]);
            }
            p
        };

        proc.current_dir(workdir)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .status()
            .context("Failed to run command")?
    };

    Ok(status.code().unwrap_or(-1))
}

/// Execute a full workflow: resolve DAG, run steps in order, collect outputs.
pub fn execute_workflow(
    workflow_path: &Path,
    workflow: &Workflow,
    dag: &[DagStep],
    inputs: &HashMap<String, ResolvedValue>,
    outdir: &Path,
) -> Result<RunResult> {
    let start_time = Utc::now();
    let log_dir = outdir.join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let workflow_dir = workflow_path.parent().unwrap_or(Path::new("."));

    // Track step outputs for wiring between steps
    let mut step_outputs: HashMap<String, HashMap<String, ResolvedValue>> = HashMap::new();
    let mut step_results = Vec::new();
    let mut success = true;

    for dag_step in dag {
        let step_def = &workflow.steps[&dag_step.name];
        let tool_path = workflow_dir.join(&step_def.run);
        let tool_doc = parse_cwl(&tool_path)
            .with_context(|| format!("Failed to parse tool: {}", tool_path.display()))?;
        let tool = match tool_doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => bail!("Step {} references a non-tool: {}", dag_step.name, step_def.run),
        };

        // Resolve step inputs from workflow inputs and upstream step outputs
        let step_inputs = resolve_step_inputs(
            &step_def.inputs,
            inputs,
            &step_outputs,
            &RuntimeContext {
                cores: 1, ram: 1024,
                outdir: outdir.to_path_buf(),
                tmpdir: std::env::temp_dir(),
            },
        )?;

        let workdir = outdir.join(format!(".steps/{}", dag_step.name));
        let runtime = RuntimeContext {
            cores: 1, ram: 1024,
            outdir: outdir.to_path_buf(),
            tmpdir: std::env::temp_dir(),
        };

        let step_start = Utc::now();
        let docker_image = crate::parse::docker_image(&tool);

        let (exit_code, outputs) = execute_tool(
            &tool, &step_inputs, &workdir, &runtime, &log_dir, &dag_step.name,
        )?;

        let step_end = Utc::now();

        let step_result = StepResult {
            step_name: dag_step.name.clone(),
            tool_path: tool_path.clone(),
            container_image: docker_image,
            start_time: step_start,
            end_time: step_end,
            exit_code,
            inputs: step_inputs,
            outputs: outputs.clone(),
            stdout_path: Some(log_dir.join(format!("{}.stdout.log", dag_step.name))),
            stderr_path: Some(log_dir.join(format!("{}.stderr.log", dag_step.name))),
        };

        if exit_code != 0 {
            eprintln!("Step '{}' failed with exit code {}", dag_step.name, exit_code);
            success = false;
            step_results.push(step_result);
            break;
        }

        step_outputs.insert(dag_step.name.clone(), outputs);
        step_results.push(step_result);
    }

    // Collect workflow-level outputs from final step outputs
    let wf_outputs = resolve_workflow_outputs(&workflow.outputs, &step_outputs);

    // Copy final output files to outdir
    for (_name, val) in &wf_outputs {
        copy_output_to_outdir(val, outdir)?;
    }

    let end_time = Utc::now();

    Ok(RunResult {
        workflow_path: workflow_path.to_path_buf(),
        workflow: workflow.clone(),
        inputs: inputs.clone(),
        outputs: wf_outputs,
        steps: step_results,
        start_time,
        end_time,
        success,
    })
}

/// Resolve step inputs from workflow inputs and upstream step outputs.
fn resolve_step_inputs(
    step_in: &HashMap<String, StepInput>,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
    runtime: &RuntimeContext,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut resolved = HashMap::new();

    for (name, input) in step_in {
        let val = match input {
            StepInput::Source(source) => resolve_source(source, wf_inputs, step_outputs),
            StepInput::Structured(entry) => {
                let base = match &entry.source {
                    Some(src) => resolve_source(src, wf_inputs, step_outputs),
                    None => entry.default.as_ref()
                        .map(|d| yaml_to_resolved(d))
                        .unwrap_or(ResolvedValue::Null),
                };
                if let Some(vf) = &entry.value_from {
                    let s = crate::param::resolve_param_refs(vf, &HashMap::new(), runtime, Some(&base));
                    ResolvedValue::String(s)
                } else {
                    base
                }
            }
        };
        resolved.insert(name.clone(), val);
    }

    Ok(resolved)
}

/// Resolve a source reference like "input_name" or "step_name/output_name".
fn resolve_source(
    source: &str,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
) -> ResolvedValue {
    if let Some((step, output)) = source.split_once('/') {
        step_outputs
            .get(step)
            .and_then(|o| o.get(output))
            .cloned()
            .unwrap_or(ResolvedValue::Null)
    } else {
        wf_inputs.get(source).cloned().unwrap_or(ResolvedValue::Null)
    }
}

/// Resolve workflow-level outputs from step outputs.
fn resolve_workflow_outputs(
    wf_outputs: &HashMap<String, WorkflowOutput>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
) -> HashMap<String, ResolvedValue> {
    let mut resolved = HashMap::new();
    for (name, output) in wf_outputs {
        if let Some((step, out_name)) = output.output_source.split_once('/') {
            let val = step_outputs
                .get(step)
                .and_then(|o| o.get(out_name))
                .cloned()
                .unwrap_or(ResolvedValue::Null);
            resolved.insert(name.clone(), val);
        }
    }
    resolved
}

/// Copy an output file to the outdir root.
fn copy_output_to_outdir(val: &ResolvedValue, outdir: &Path) -> Result<()> {
    match val {
        ResolvedValue::File(f) => {
            let dest = outdir.join(&f.basename);
            if f.path != dest && f.path.exists() {
                std::fs::copy(&f.path, &dest)?;
            }
            for sf in &f.secondary_files {
                copy_output_to_outdir(&ResolvedValue::File(sf.clone()), outdir)?;
            }
        }
        ResolvedValue::Array(arr) => {
            for v in arr {
                copy_output_to_outdir(v, outdir)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Convert a serde_yaml::Value to a ResolvedValue (for defaults).
fn yaml_to_resolved(val: &serde_yaml::Value) -> ResolvedValue {
    match val {
        serde_yaml::Value::Null => ResolvedValue::Null,
        serde_yaml::Value::Bool(b) => ResolvedValue::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() { ResolvedValue::Int(i) }
            else if let Some(f) = n.as_f64() { ResolvedValue::Float(f) }
            else { ResolvedValue::String(n.to_string()) }
        }
        serde_yaml::Value::String(s) => ResolvedValue::String(s.clone()),
        _ => ResolvedValue::String(serde_yaml::to_string(val).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_source_workflow_input() {
        let mut wf_inputs = HashMap::new();
        wf_inputs.insert("sample_id".into(), ResolvedValue::String("SRX123".into()));
        let step_outputs = HashMap::new();
        let val = resolve_source("sample_id", &wf_inputs, &step_outputs);
        assert!(matches!(val, ResolvedValue::String(s) if s == "SRX123"));
    }

    #[test]
    fn resolve_source_step_output() {
        let wf_inputs = HashMap::new();
        let mut step_outputs = HashMap::new();
        let mut align_outs = HashMap::new();
        align_outs.insert("aligned_sam".into(), ResolvedValue::File(
            FileValue::from_path(PathBuf::from("/work/sample.sam"))
        ));
        step_outputs.insert("align".into(), align_outs);
        let val = resolve_source("align/aligned_sam", &wf_inputs, &step_outputs);
        assert!(matches!(val, ResolvedValue::File(f) if f.basename == "sample.sam"));
    }

    #[test]
    fn resolve_step_inputs_with_default() {
        let mut step_in = HashMap::new();
        step_in.insert("by_name".into(), StepInput::Structured(StepInputEntry {
            source: None,
            value_from: None,
            default: Some(serde_yaml::Value::Bool(true)),
        }));
        let wf_inputs = HashMap::new();
        let step_outputs = HashMap::new();
        let rt = RuntimeContext {
            cores: 1, ram: 1024,
            outdir: PathBuf::from("/out"),
            tmpdir: PathBuf::from("/tmp"),
        };
        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &rt).unwrap();
        assert!(matches!(resolved.get("by_name"), Some(ResolvedValue::Bool(true))));
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add `pub mod execute;`

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test execute
```

Expected: all execute tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/execute.rs src/lib.rs
git commit -m "feat: step executor with Docker support and workflow orchestration"
```

---

## Task 9: RO-Crate Provenance — Boilerplate and Workflow Entities

**Files:**
- Create: `src/provenance.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Implement boilerplate and workflow-level entities with tests**

`src/provenance.rs`:
```rust
use crate::execute::{RunResult, StepResult};
use crate::model::*;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const PROCESS_CRATE_URI: &str = "https://w3id.org/ro/wfrun/process/0.5";
const WORKFLOW_CRATE_URI: &str = "https://w3id.org/ro/wfrun/workflow/0.5";
const PROVENANCE_CRATE_URI: &str = "https://w3id.org/ro/wfrun/provenance/0.5";
const WORKFLOW_RO_CRATE_URI: &str = "https://w3id.org/workflowhub/workflow-ro-crate/1.0";
const RO_CRATE_URI: &str = "https://w3id.org/ro/crate/1.1";
const CWL_LANGUAGE_URI: &str = "https://w3id.org/workflowhub/workflow-ro-crate#cwl";
const CWL_ZEN_URL: &str = "https://github.com/inutano/cwl-zen";

/// Generate a Provenance Run Crate and write ro-crate-metadata.json to outdir.
pub fn generate_crate(run_result: &RunResult, outdir: &Path) -> Result<()> {
    let mut graph = Vec::new();

    add_boilerplate(&mut graph, run_result);
    add_workflow_entity(&mut graph, run_result, outdir)?;
    add_formal_parameters(&mut graph, &run_result.workflow);
    let run_id = add_workflow_action(&mut graph, run_result, outdir)?;
    let org_id = add_organize_action(&mut graph, run_result, &run_id);
    add_step_actions(&mut graph, run_result, outdir, &org_id)?;
    add_data_entities(&mut graph, run_result, outdir)?;
    enrich_with_tataki(&mut graph, outdir);

    // Build hasPart and mentions for root dataset
    finalize_root_dataset(&mut graph, &run_id);

    let metadata = json!({
        "@context": [
            "https://w3id.org/ro/crate/1.1/context",
            "https://w3id.org/ro/terms/workflow-run/context"
        ],
        "@graph": graph,
    });

    let path = outdir.join("ro-crate-metadata.json");
    let content = serde_json::to_string_pretty(&metadata)?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    // Copy workflow and tool files into the crate
    copy_workflow_files(run_result, outdir)?;

    Ok(())
}

fn add_boilerplate(graph: &mut Vec<Value>, _run_result: &RunResult) {
    // Metadata descriptor
    graph.push(json!({
        "@id": "ro-crate-metadata.json",
        "@type": "CreativeWork",
        "about": {"@id": "./"},
        "conformsTo": [
            {"@id": RO_CRATE_URI},
            {"@id": WORKFLOW_RO_CRATE_URI}
        ]
    }));

    // Root dataset (hasPart and mentions filled in later by finalize_root_dataset)
    graph.push(json!({
        "@id": "./",
        "@type": "Dataset",
        "conformsTo": [
            {"@id": PROCESS_CRATE_URI},
            {"@id": WORKFLOW_CRATE_URI},
            {"@id": PROVENANCE_CRATE_URI},
            {"@id": WORKFLOW_RO_CRATE_URI}
        ],
        "datePublished": format_timestamp(&Utc::now()),
        "license": "https://spdx.org/licenses/MIT"
    }));

    // Profile stubs
    for (uri, name, version) in [
        (PROCESS_CRATE_URI, "Process Run Crate", "0.5"),
        (WORKFLOW_CRATE_URI, "Workflow Run Crate", "0.5"),
        (PROVENANCE_CRATE_URI, "Provenance Run Crate", "0.5"),
        (WORKFLOW_RO_CRATE_URI, "Workflow RO-Crate", "1.0"),
    ] {
        graph.push(json!({
            "@id": uri,
            "@type": "CreativeWork",
            "name": name,
            "version": version
        }));
    }

    // CWL language
    graph.push(json!({
        "@id": CWL_LANGUAGE_URI,
        "@type": "ComputerLanguage",
        "name": "Common Workflow Language",
        "alternateName": "CWL",
        "identifier": {"@id": "https://w3id.org/cwl/v1.2/"},
        "url": {"@id": "https://www.commonwl.org/"},
        "version": "v1.2"
    }));

    // cwl-zen engine
    graph.push(json!({
        "@id": CWL_ZEN_URL,
        "@type": "SoftwareApplication",
        "name": "cwl-zen",
        "version": env!("CARGO_PKG_VERSION"),
        "url": CWL_ZEN_URL
    }));
}

fn add_workflow_entity(
    graph: &mut Vec<Value>,
    run_result: &RunResult,
    _outdir: &Path,
) -> Result<()> {
    let wf_filename = run_result.workflow_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let input_params: Vec<Value> = run_result.workflow.inputs.keys()
        .map(|k| json!({"@id": format!("#param/input/{}", k)}))
        .collect();

    let output_params: Vec<Value> = run_result.workflow.outputs.keys()
        .map(|k| json!({"@id": format!("#param/output/{}", k)}))
        .collect();

    graph.push(json!({
        "@id": wf_filename,
        "@type": ["File", "SoftwareSourceCode", "ComputationalWorkflow"],
        "name": run_result.workflow.label.as_deref().unwrap_or(&wf_filename),
        "programmingLanguage": {"@id": CWL_LANGUAGE_URI},
        "input": input_params,
        "output": output_params
    }));

    Ok(())
}

fn add_formal_parameters(graph: &mut Vec<Value>, workflow: &Workflow) {
    // Input parameters
    for (name, input) in &workflow.inputs {
        let schema_type = cwl_type_to_schema_org(&input.cwl_type);
        let mut param = json!({
            "@id": format!("#param/input/{}", name),
            "@type": "FormalParameter",
            "name": name,
            "additionalType": schema_type
        });
        if input.cwl_type.is_optional() {
            param["valueRequired"] = json!(false);
        }
        if input.cwl_type.is_array() {
            param["multipleValues"] = json!(true);
        }
        graph.push(param);
    }

    // Output parameters
    for (name, output) in &workflow.outputs {
        let schema_type = cwl_type_to_schema_org(&output.cwl_type);
        let mut param = json!({
            "@id": format!("#param/output/{}", name),
            "@type": "FormalParameter",
            "name": name,
            "additionalType": schema_type
        });
        if output.cwl_type.is_optional() {
            param["valueRequired"] = json!(false);
        }
        graph.push(param);
    }
}

/// Map CWL types to Schema.org additionalType values.
fn cwl_type_to_schema_org(cwl_type: &CwlType) -> &'static str {
    match cwl_type.base_type() {
        "File" => "File",
        "Directory" => "Dataset",
        "string" => "Text",
        "int" | "long" => "Integer",
        "float" | "double" => "Float",
        "boolean" => "Boolean",
        _ => "Text",
    }
}

fn add_workflow_action(
    graph: &mut Vec<Value>,
    run_result: &RunResult,
    _outdir: &Path,
) -> Result<String> {
    let run_id = format!("#urn:uuid:{}", uuid::Uuid::new_v4());

    let wf_filename = run_result.workflow_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let status = if run_result.success {
        "http://schema.org/CompletedActionStatus"
    } else {
        "http://schema.org/FailedActionStatus"
    };

    let input_refs: Vec<Value> = run_result.inputs.keys()
        .map(|k| json!({"@id": format!("#pv/input/{}", k)}))
        .collect();

    let output_refs: Vec<Value> = run_result.outputs.keys()
        .map(|k| json!({"@id": format!("#data/output/{}", k)}))
        .collect();

    graph.push(json!({
        "@id": &run_id,
        "@type": "CreateAction",
        "name": format!("cwl-zen run {}", wf_filename),
        "instrument": {"@id": wf_filename},
        "object": input_refs,
        "result": output_refs,
        "startTime": format_timestamp(&run_result.start_time),
        "endTime": format_timestamp(&run_result.end_time),
        "actionStatus": status
    }));

    Ok(run_id)
}

fn add_organize_action(
    graph: &mut Vec<Value>,
    run_result: &RunResult,
    run_id: &str,
) -> String {
    let org_id = format!("#urn:uuid:{}", uuid::Uuid::new_v4());

    let step_control_refs: Vec<Value> = run_result.steps.iter()
        .map(|s| json!({"@id": format!("#control/{}", s.step_name)}))
        .collect();

    graph.push(json!({
        "@id": &org_id,
        "@type": "OrganizeAction",
        "name": "cwl-zen orchestration",
        "instrument": {"@id": CWL_ZEN_URL},
        "object": {"@id": run_id},
        "result": step_control_refs,
        "startTime": format_timestamp(&run_result.start_time),
        "endTime": format_timestamp(&run_result.end_time)
    }));

    org_id
}

fn add_step_actions(
    graph: &mut Vec<Value>,
    run_result: &RunResult,
    _outdir: &Path,
    org_id: &str,
) -> Result<()> {
    for (i, step) in run_result.steps.iter().enumerate() {
        let step_action_id = format!("#step/{}", step.step_name);
        let tool_filename = step.tool_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let tool_crate_path = format!("tools/{}", tool_filename);

        let status = if step.exit_code == 0 {
            "http://schema.org/CompletedActionStatus"
        } else {
            "http://schema.org/FailedActionStatus"
        };

        // Step CreateAction
        let step_input_refs: Vec<Value> = step.inputs.keys()
            .map(|k| json!({"@id": format!("#data/step/{}/{}", step.step_name, k)}))
            .collect();
        let step_output_refs: Vec<Value> = step.outputs.keys()
            .map(|k| json!({"@id": format!("#data/step/{}/{}", step.step_name, k)}))
            .collect();

        let mut action = json!({
            "@id": &step_action_id,
            "@type": "CreateAction",
            "name": format!("Run {}", step.step_name),
            "instrument": {"@id": &tool_crate_path},
            "object": step_input_refs,
            "result": step_output_refs,
            "startTime": format_timestamp(&step.start_time),
            "endTime": format_timestamp(&step.end_time),
            "actionStatus": status,
            "exitCode": step.exit_code
        });

        if let Some(ref image) = step.container_image {
            let image_id = format!("#container/{}", step.step_name);
            action["containerImage"] = json!({"@id": &image_id});
            graph.push(json!({
                "@id": &image_id,
                "@type": "ContainerImage",
                "name": image
            }));
        }

        // Log files
        let mut subject_of = Vec::new();
        if let Some(ref path) = step.stdout_path {
            if path.exists() {
                let log_id = format!("logs/{}.stdout.log", step.step_name);
                subject_of.push(json!({"@id": &log_id}));
            }
        }
        if let Some(ref path) = step.stderr_path {
            if path.exists() {
                let log_id = format!("logs/{}.stderr.log", step.step_name);
                subject_of.push(json!({"@id": &log_id}));
            }
        }
        if !subject_of.is_empty() {
            action["subjectOf"] = json!(subject_of);
        }

        graph.push(action);

        // Tool as SoftwareApplication
        graph.push(json!({
            "@id": &tool_crate_path,
            "@type": ["File", "SoftwareApplication"],
            "name": tool_filename
        }));

        // HowToStep
        graph.push(json!({
            "@id": format!("#howto/{}", step.step_name),
            "@type": "HowToStep",
            "name": &step.step_name,
            "position": i + 1
        }));

        // ControlAction linking OrganizeAction to step
        graph.push(json!({
            "@id": format!("#control/{}", step.step_name),
            "@type": "ControlAction",
            "instrument": {"@id": format!("#howto/{}", step.step_name)},
            "object": {"@id": &step_action_id}
        }));
    }

    Ok(())
}

fn add_data_entities(
    graph: &mut Vec<Value>,
    run_result: &RunResult,
    outdir: &Path,
) -> Result<()> {
    // Workflow-level inputs
    for (name, val) in &run_result.inputs {
        let param_id = format!("#param/input/{}", name);
        match val {
            ResolvedValue::File(f) => {
                let entity_id = format!("#pv/input/{}", name);
                let mut entity = file_entity(&entity_id, f, outdir)?;
                entity["exampleOfWork"] = json!({"@id": param_id});
                graph.push(entity);
            }
            _ => {
                graph.push(json!({
                    "@id": format!("#pv/input/{}", name),
                    "@type": "PropertyValue",
                    "name": name,
                    "value": crate::param::value_to_string(val),
                    "exampleOfWork": {"@id": param_id}
                }));
            }
        }
    }

    // Workflow-level outputs
    for (name, val) in &run_result.outputs {
        let param_id = format!("#param/output/{}", name);
        if let ResolvedValue::File(f) = val {
            let entity_id = format!("#data/output/{}", name);
            let mut entity = file_entity(&entity_id, f, outdir)?;
            entity["exampleOfWork"] = json!({"@id": param_id});
            graph.push(entity);
        }
    }

    // Per-step inputs and outputs (intermediate files)
    for step in &run_result.steps {
        for (name, val) in &step.inputs {
            if let ResolvedValue::File(f) = val {
                let entity_id = format!("#data/step/{}/{}", step.step_name, name);
                let entity = file_entity(&entity_id, f, outdir)?;
                graph.push(entity);
            }
        }
        for (name, val) in &step.outputs {
            if let ResolvedValue::File(f) = val {
                let entity_id = format!("#data/step/{}/{}", step.step_name, name);
                let entity = file_entity(&entity_id, f, outdir)?;
                graph.push(entity);
            }
        }

        // Log file entities
        for (suffix, path_opt) in [("stdout", &step.stdout_path), ("stderr", &step.stderr_path)] {
            if let Some(path) = path_opt {
                if path.exists() {
                    let log_id = format!("logs/{}.{}.log", step.step_name, suffix);
                    graph.push(json!({
                        "@id": log_id,
                        "@type": "File",
                        "name": format!("{} {} log", step.step_name, suffix),
                        "encodingFormat": "text/plain"
                    }));
                }
            }
        }
    }

    Ok(())
}

fn file_entity(id: &str, file: &FileValue, _outdir: &Path) -> Result<Value> {
    let mut entity = json!({
        "@id": id,
        "@type": "File",
        "name": file.basename,
        "contentSize": file.size,
        "dateModified": format_timestamp(&Utc::now())
    });

    if file.path.exists() {
        entity["sha256"] = json!(compute_sha256(&file.path)?);
    }

    if let Some((edam_id, label)) = extension_to_edam(&file.nameext) {
        entity["encodingFormat"] = json!({
            "@id": format!("http://edamontology.org/{}", edam_id),
            "@type": "Thing",
            "name": label
        });
    }

    Ok(entity)
}

fn enrich_with_tataki(graph: &mut Vec<Value>, outdir: &Path) {
    // Try running tataki on output files
    let output_files: Vec<PathBuf> = std::fs::read_dir(outdir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| e.file_name().to_string_lossy() != "ro-crate-metadata.json")
        .map(|e| e.path())
        .collect();

    if output_files.is_empty() {
        return;
    }

    let tataki = std::process::Command::new("tataki")
        .arg("-f").arg("json")
        .arg("-q")
        .args(&output_files)
        .output();

    let output = match tataki {
        Ok(o) if o.status.success() => o.stdout,
        _ => return, // tataki not available or failed — silently skip
    };

    let detections: HashMap<String, Value> = match serde_json::from_slice(&output) {
        Ok(d) => d,
        Err(_) => return,
    };

    for (path_str, detection) in &detections {
        let edam_id = detection.get("id").and_then(|v| v.as_str());
        let edam_label = detection.get("label").and_then(|v| v.as_str());
        if let (Some(id), Some(label)) = (edam_id, edam_label) {
            // Find matching entity in graph and replace encodingFormat
            let filename = Path::new(path_str)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            for entity in graph.iter_mut() {
                if let Some(name) = entity.get("name").and_then(|v| v.as_str()) {
                    if name == filename {
                        entity["encodingFormat"] = json!({
                            "@id": id,
                            "@type": "Thing",
                            "name": label
                        });
                    }
                }
            }
        }
    }
}

fn finalize_root_dataset(graph: &mut Vec<Value>, run_id: &str) {
    // Collect all data entity @ids for hasPart
    let has_part: Vec<Value> = graph.iter()
        .filter(|e| {
            let id = e.get("@id").and_then(|v| v.as_str()).unwrap_or("");
            let types = e.get("@type");
            // Include File entities and PropertyValue entities
            (types == Some(&json!("File"))
                || types == Some(&json!("PropertyValue"))
                || (types.is_some() && types.unwrap().as_array().map_or(false, |a| a.contains(&json!("File")))))
                && id != "ro-crate-metadata.json"
        })
        .filter_map(|e| e.get("@id").cloned())
        .map(|id| json!({"@id": id}))
        .collect();

    // Find root dataset and update
    for entity in graph.iter_mut() {
        if entity.get("@id") == Some(&json!("./")) {
            entity["hasPart"] = json!(has_part);
            entity["mentions"] = json!([{"@id": run_id}]);
            // Find workflow entity for mainEntity
            for e in graph.iter() {
                let types = e.get("@type");
                if let Some(arr) = types.and_then(|t| t.as_array()) {
                    if arr.contains(&json!("ComputationalWorkflow")) {
                        entity["mainEntity"] = json!({"@id": e["@id"]});
                        break;
                    }
                }
            }
            break;
        }
    }
}

/// Format a DateTime as ISO 8601 with +00:00 offset (not Z).
fn format_timestamp(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// Compute SHA-256 hash of a file.
fn compute_sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Map file extension to EDAM format ID and label.
fn extension_to_edam(ext: &str) -> Option<(&'static str, &'static str)> {
    match ext.to_lowercase().as_str() {
        ".bam" => Some(("format_2572", "BAM")),
        ".sam" => Some(("format_2573", "SAM")),
        ".cram" => Some(("format_3462", "CRAM")),
        ".vcf" | ".vcf.gz" => Some(("format_3016", "VCF")),
        ".bcf" => Some(("format_3020", "BCF")),
        ".fastq" | ".fq" | ".fastq.gz" | ".fq.gz" => Some(("format_1930", "FASTQ")),
        ".fasta" | ".fa" | ".fasta.gz" | ".fa.gz" => Some(("format_1929", "FASTA")),
        ".bed" => Some(("format_3003", "BED")),
        ".gff3" | ".gff" => Some(("format_1975", "GFF3")),
        ".gtf" => Some(("format_2306", "GTF")),
        ".bw" | ".bigwig" => Some(("format_3006", "bigWig")),
        ".bb" | ".bigbed" => Some(("format_3004", "bigBed")),
        _ => None,
    }
}

/// Copy workflow and tool CWL files into the crate directory.
fn copy_workflow_files(run_result: &RunResult, outdir: &Path) -> Result<()> {
    // Copy main workflow
    let wf_dest = outdir.join(run_result.workflow_path.file_name().unwrap_or_default());
    if run_result.workflow_path.exists() && !wf_dest.exists() {
        std::fs::copy(&run_result.workflow_path, &wf_dest)?;
    }

    // Copy tool files
    let tools_dir = outdir.join("tools");
    std::fs::create_dir_all(&tools_dir)?;
    for step in &run_result.steps {
        let tool_dest = tools_dir.join(step.tool_path.file_name().unwrap_or_default());
        if step.tool_path.exists() && !tool_dest.exists() {
            std::fs::copy(&step.tool_path, &tool_dest)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_no_z() {
        let dt = chrono::DateTime::parse_from_rfc3339("2024-01-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = format_timestamp(&dt);
        assert!(!s.ends_with('Z'));
        assert!(s.ends_with("+00:00"));
    }

    #[test]
    fn edam_mapping() {
        assert_eq!(extension_to_edam(".bam"), Some(("format_2572", "BAM")));
        assert_eq!(extension_to_edam(".fastq"), Some(("format_1930", "FASTQ")));
        assert_eq!(extension_to_edam(".txt"), None);
    }

    #[test]
    fn cwl_type_mapping() {
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("File".into())), "File");
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("string".into())), "Text");
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("int".into())), "Integer");
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("float".into())), "Float");
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("boolean".into())), "Boolean");
        assert_eq!(cwl_type_to_schema_org(&CwlType::Single("Directory".into())), "Dataset");
    }

    #[test]
    fn boilerplate_has_required_entities() {
        let mut graph = Vec::new();
        let run_result = mock_run_result();
        add_boilerplate(&mut graph, &run_result);

        // Should have: metadata descriptor, root dataset, 4 profiles, CWL language, cwl-zen app
        assert!(graph.len() >= 8);

        // Metadata descriptor
        let meta = graph.iter().find(|e| e["@id"] == "ro-crate-metadata.json").unwrap();
        assert_eq!(meta["@type"], "CreativeWork");

        // Root dataset has all 4 conformsTo
        let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
        let conforms = root["conformsTo"].as_array().unwrap();
        assert_eq!(conforms.len(), 4);

        // CWL language entity
        let lang = graph.iter().find(|e| e["@id"] == CWL_LANGUAGE_URI).unwrap();
        assert_eq!(lang["name"], "Common Workflow Language");
    }

    #[test]
    fn formal_parameters_created() {
        let mut graph = Vec::new();
        let wf = mock_workflow();
        add_formal_parameters(&mut graph, &wf);

        // Should have FormalParameter for each input and output
        let params: Vec<_> = graph.iter()
            .filter(|e| e["@type"] == "FormalParameter")
            .collect();
        // 2 inputs (sample_id, reads) + 1 output (bam)
        assert_eq!(params.len(), 3);

        let sample_param = params.iter().find(|p| p["name"] == "sample_id").unwrap();
        assert_eq!(sample_param["additionalType"], "Text");

        let reads_param = params.iter().find(|p| p["name"] == "reads").unwrap();
        assert_eq!(reads_param["additionalType"], "File");
    }

    // --- Test helpers ---

    fn mock_workflow() -> Workflow {
        let mut inputs = HashMap::new();
        inputs.insert("sample_id".into(), WorkflowInput {
            cwl_type: CwlType::Single("string".into()),
            secondary_files: vec![],
            doc: None,
            default: None,
        });
        inputs.insert("reads".into(), WorkflowInput {
            cwl_type: CwlType::Single("File".into()),
            secondary_files: vec![],
            doc: None,
            default: None,
        });

        let mut outputs = HashMap::new();
        outputs.insert("bam".into(), WorkflowOutput {
            cwl_type: CwlType::Single("File".into()),
            output_source: "sort/sorted_bam".into(),
            doc: None,
        });

        Workflow {
            cwl_version: Some("v1.2".into()),
            label: Some("Test workflow".into()),
            doc: None,
            inputs,
            outputs,
            steps: HashMap::new(),
            requirements: HashMap::new(),
        }
    }

    fn mock_run_result() -> RunResult {
        RunResult {
            workflow_path: PathBuf::from("workflow.cwl"),
            workflow: mock_workflow(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            steps: vec![],
            start_time: Utc::now(),
            end_time: Utc::now(),
            success: true,
        }
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add `pub mod provenance;`

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test provenance
```

Expected: all provenance tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/provenance.rs src/lib.rs
git commit -m "feat: RO-Crate Provenance Run Crate generation"
```

---

## Task 10: CLI with clap

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Implement CLI**

`src/main.rs`:
```rust
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use cwl_zen::dag;
use cwl_zen::execute;
use cwl_zen::input;
use cwl_zen::model::CwlDocument;
use cwl_zen::parse;
use cwl_zen::provenance;

#[derive(Parser)]
#[command(name = "cwl-zen", version, about = "A minimal, JS-free CWL v1.2 runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a CWL workflow or tool
    Run {
        /// CWL workflow or tool file
        cwl_file: PathBuf,
        /// Input YAML file
        input_file: PathBuf,
        /// Output directory
        #[arg(long, default_value = "./cwl-zen-output")]
        outdir: PathBuf,
        /// Suppress RO-Crate provenance generation
        #[arg(long)]
        no_crate: bool,
    },
    /// Validate a CWL document
    Validate {
        /// CWL file(s) to validate
        files: Vec<PathBuf>,
    },
    /// Print the step dependency graph
    Dag {
        /// CWL workflow file
        cwl_file: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { cwl_file, input_file, outdir, no_crate } => {
            cmd_run(&cwl_file, &input_file, &outdir, no_crate)
        }
        Commands::Validate { files } => cmd_validate(&files),
        Commands::Dag { cwl_file } => cmd_dag(&cwl_file),
    }
}

fn cmd_run(cwl_file: &PathBuf, input_file: &PathBuf, outdir: &PathBuf, no_crate: bool) -> Result<()> {
    let doc = parse::parse_cwl(cwl_file)?;
    let input_dir = input_file.parent().unwrap_or(std::path::Path::new("."));
    let inputs = input::parse_inputs(input_file, input_dir)?;

    std::fs::create_dir_all(outdir)?;

    match doc {
        CwlDocument::Workflow(wf) => {
            let dag_steps = dag::build_dag(&wf)?;
            eprintln!("Executing workflow: {} ({} steps)", cwl_file.display(), dag_steps.len());
            for step in &dag_steps {
                eprintln!("  -> {}", step.name);
            }

            let run_result = execute::execute_workflow(cwl_file, &wf, &dag_steps, &inputs, outdir)?;

            if !no_crate {
                provenance::generate_crate(&run_result, outdir)?;
                eprintln!("Provenance: {}/ro-crate-metadata.json", outdir.display());
            }

            if run_result.success {
                eprintln!("Workflow completed successfully.");
            } else {
                eprintln!("Workflow failed.");
                std::process::exit(1);
            }
        }
        CwlDocument::CommandLineTool(tool) => {
            eprintln!("Executing tool: {}", cwl_file.display());
            let runtime = cwl_zen::model::RuntimeContext {
                cores: 1, ram: 1024,
                outdir: outdir.to_path_buf(),
                tmpdir: std::env::temp_dir(),
            };
            let (exit_code, outputs) = execute::execute_tool(
                &tool, &inputs, outdir, &runtime, &outdir.join("logs"), "tool",
            )?;

            if exit_code == 0 {
                eprintln!("Tool completed successfully.");
                for (name, val) in &outputs {
                    eprintln!("  {}: {}", name, cwl_zen::param::value_to_string(val));
                }
            } else {
                eprintln!("Tool failed with exit code {}.", exit_code);
                std::process::exit(exit_code);
            }
        }
    }

    Ok(())
}

fn cmd_validate(files: &[PathBuf]) -> Result<()> {
    let mut errors = 0;
    for file in files {
        match parse::parse_cwl(file) {
            Ok(doc) => {
                let class = match &doc {
                    CwlDocument::CommandLineTool(_) => "CommandLineTool",
                    CwlDocument::Workflow(_) => "Workflow",
                };
                eprintln!("PASS: {} ({})", file.display(), class);
            }
            Err(e) => {
                eprintln!("FAIL: {} — {}", file.display(), e);
                errors += 1;
            }
        }
    }
    if errors > 0 {
        bail!("{} file(s) failed validation", errors);
    }
    Ok(())
}

fn cmd_dag(cwl_file: &PathBuf) -> Result<()> {
    let doc = parse::parse_cwl(cwl_file)?;
    match doc {
        CwlDocument::Workflow(wf) => {
            let steps = dag::build_dag(&wf)?;
            eprintln!("DAG for {}:", cwl_file.display());
            dag::print_dag(&steps);
        }
        _ => bail!("{} is not a Workflow", cwl_file.display()),
    }
    Ok(())
}
```

- [ ] **Step 2: Build and verify**

```bash
cd ~/repos/cwl-zen && cargo build
```

Expected: compiles without errors.

```bash
cd ~/repos/cwl-zen && cargo run -- validate tests/fixtures/echo.cwl tests/fixtures/two-step.cwl
```

Expected: both PASS.

```bash
cd ~/repos/cwl-zen && cargo run -- dag tests/fixtures/two-step.cwl
```

Expected: prints DAG with echo_step → cat_step.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: CLI with run, validate, and dag subcommands"
```

---

## Task 11: Integration Test — Echo Tool

**Files:**
- Create: `tests/integration_echo.rs`

- [ ] **Step 1: Write integration test for the echo tool**

`tests/integration_echo.rs`:
```rust
use std::path::Path;
use std::collections::HashMap;

#[test]
fn run_echo_tool() {
    let doc = cwl_zen::parse::parse_cwl(Path::new("tests/fixtures/echo.cwl")).unwrap();
    let tool = match doc {
        cwl_zen::model::CwlDocument::CommandLineTool(t) => t,
        _ => panic!("Expected CommandLineTool"),
    };

    let mut inputs = HashMap::new();
    inputs.insert("message".into(), cwl_zen::model::ResolvedValue::String("hello-zen".into()));

    let outdir = tempfile::tempdir().unwrap();
    let runtime = cwl_zen::model::RuntimeContext {
        cores: 1,
        ram: 1024,
        outdir: outdir.path().to_path_buf(),
        tmpdir: std::env::temp_dir(),
    };

    let (exit_code, outputs) = cwl_zen::execute::execute_tool(
        &tool, &inputs, outdir.path(), &runtime, &outdir.path().join("logs"), "echo",
    ).unwrap();

    assert_eq!(exit_code, 0);

    // Check stdout was captured
    let stdout_log = outdir.path().join("logs/echo.stdout.log");
    let content = std::fs::read_to_string(&stdout_log).unwrap();
    assert!(content.contains("hello-zen"));
}
```

- [ ] **Step 2: Add tempfile dependency**

In `Cargo.toml`, add under `[dev-dependencies]`:
```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run test**

```bash
cd ~/repos/cwl-zen && cargo test integration_echo -- --nocapture
```

Expected: PASS — echo runs, stdout captured.

- [ ] **Step 4: Commit**

```bash
git add tests/integration_echo.rs Cargo.toml
git commit -m "test: integration test for echo tool execution"
```

---

## Task 12: Integration Test — Two-Step Workflow with Provenance

**Files:**
- Create: `tests/integration_workflow.rs`

- [ ] **Step 1: Write integration test for the two-step workflow**

`tests/integration_workflow.rs`:
```rust
use std::path::Path;

#[test]
fn run_two_step_workflow_with_provenance() {
    let cwl_file = Path::new("tests/fixtures/two-step.cwl");
    let doc = cwl_zen::parse::parse_cwl(cwl_file).unwrap();
    let wf = match doc {
        cwl_zen::model::CwlDocument::Workflow(w) => w,
        _ => panic!("Expected Workflow"),
    };

    let dag = cwl_zen::dag::build_dag(&wf).unwrap();
    assert_eq!(dag.len(), 2);

    let inputs = cwl_zen::input::parse_inputs(
        Path::new("tests/fixtures/two-step-input.yml"),
        Path::new("tests/fixtures"),
    ).unwrap();

    let outdir = tempfile::tempdir().unwrap();

    let run_result = cwl_zen::execute::execute_workflow(
        cwl_file, &wf, &dag, &inputs, outdir.path(),
    ).unwrap();

    assert!(run_result.success);
    assert_eq!(run_result.steps.len(), 2);

    // Generate RO-Crate
    cwl_zen::provenance::generate_crate(&run_result, outdir.path()).unwrap();

    // Verify ro-crate-metadata.json exists and is valid JSON
    let crate_path = outdir.path().join("ro-crate-metadata.json");
    assert!(crate_path.exists());

    let content = std::fs::read_to_string(&crate_path).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Verify structure
    let graph = metadata["@graph"].as_array().unwrap();
    assert!(!graph.is_empty());

    // Has root dataset with conformsTo Provenance Run Crate
    let root = graph.iter().find(|e| e["@id"] == "./").unwrap();
    let conforms: Vec<&str> = root["conformsTo"].as_array().unwrap()
        .iter()
        .filter_map(|c| c["@id"].as_str())
        .collect();
    assert!(conforms.contains(&"https://w3id.org/ro/wfrun/provenance/0.5"));

    // Has CreateAction
    let actions: Vec<_> = graph.iter()
        .filter(|e| e["@type"] == "CreateAction")
        .collect();
    assert!(actions.len() >= 3); // 1 workflow + 2 steps

    // Has OrganizeAction
    let org = graph.iter().find(|e| e["@type"] == "OrganizeAction");
    assert!(org.is_some());

    // Has ControlAction per step
    let controls: Vec<_> = graph.iter()
        .filter(|e| e["@type"] == "ControlAction")
        .collect();
    assert_eq!(controls.len(), 2);

    // Has HowToStep per step
    let howtos: Vec<_> = graph.iter()
        .filter(|e| e["@type"] == "HowToStep")
        .collect();
    assert_eq!(howtos.len(), 2);

    // Timestamps don't end with Z
    for action in &actions {
        if let Some(st) = action.get("startTime").and_then(|v| v.as_str()) {
            assert!(!st.ends_with('Z'), "Timestamp should not end with Z: {}", st);
            assert!(st.ends_with("+00:00"), "Timestamp should end with +00:00: {}", st);
        }
    }
}
```

- [ ] **Step 2: Run test**

```bash
cd ~/repos/cwl-zen && cargo test integration_workflow -- --nocapture
```

Expected: PASS — workflow runs, RO-Crate generated with Profile C entities.

- [ ] **Step 3: Commit**

```bash
git add tests/integration_workflow.rs
git commit -m "test: integration test for two-step workflow with Provenance Run Crate"
```

---

## Task 13: Polish and Full Test Suite

**Files:**
- Modify: `src/lib.rs` (ensure all modules exported)
- Run: full test suite

- [ ] **Step 1: Run all tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

Fix any failures.

- [ ] **Step 2: Run clippy**

```bash
cd ~/repos/cwl-zen && cargo clippy -- -W warnings
```

Fix any warnings.

- [ ] **Step 3: Verify binary works end-to-end**

```bash
cd ~/repos/cwl-zen && cargo run -- run tests/fixtures/echo.cwl tests/fixtures/echo-input.yml --outdir /tmp/cwl-zen-test
ls /tmp/cwl-zen-test/
cat /tmp/cwl-zen-test/ro-crate-metadata.json | python3 -m json.tool | head -30
```

Expected: output directory contains ro-crate-metadata.json and output files.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: polish, clippy fixes, full test suite green"
```

---

## Summary

| Task | Module | What it produces |
|------|--------|------------------|
| 1 | model.rs | CWL type definitions |
| 2 | parse.rs | YAML → typed structs |
| 3 | param.rs | `$(inputs.X)` resolution |
| 4 | input.rs | Input YAML → ResolvedValue |
| 5 | dag.rs | Topological sort of steps |
| 6 | command.rs | Tool → shell/Docker command |
| 7 | stage.rs | File staging, glob collection |
| 8 | execute.rs | Step/workflow execution |
| 9 | provenance.rs | RO-Crate Provenance Run Crate |
| 10 | main.rs | CLI (run, validate, dag) |
| 11 | integration | Echo tool test |
| 12 | integration | Two-step workflow + provenance test |
| 13 | polish | Clippy, full suite, end-to-end |
| 14 | scatter.rs | Scatter/dotproduct expansion |
| 15 | fixes | Review fixes: separate, shellQuote, NetworkAccess, dateModified, lint |

---

## Known Gaps and Required Fixes

These were identified during plan self-review and must be addressed during implementation. The implementing agent should apply these fixes inline in the relevant tasks rather than as a separate pass.

### Critical Fixes

**1. Scatter/dotproduct support (add Task 14)**

The spec supports `scatter` with `scatterMethod: dotproduct`. This requires a `scatter.rs` module that:
- Detects scatter fields on a WorkflowStep
- Expands array inputs into parallel invocations (dotproduct = zip element-wise)
- Collects array outputs from all invocations
- `execute_workflow()` must call scatter expansion before running each step

The scatter fixture `tests/fixtures/scatter-workflow.cwl` should test this. Without scatter, the ChIP-Atlas reference pipeline cannot run.

**2. `finalize_root_dataset` borrow checker violation (Task 9)**

The function iterates `graph.iter()` to collect `has_part`, then `graph.iter_mut()` to update root, then inside that tries `graph.iter()` again. This won't compile. Fix: collect all needed data (has_part, mainEntity id) in a first pass, then apply mutations in a second pass.

**3. `resolve_step_inputs` passes empty HashMap (Task 8)**

Line `crate::param::resolve_param_refs(vf, &HashMap::new(), runtime, Some(&base))` should pass the resolved workflow-level inputs, not an empty map. Otherwise `$(inputs.X)` in `valueFrom` won't resolve.

**4. `lint` subcommand missing from CLI (Task 10)**

Add a `Lint` variant to the CLI that shells out to the existing Python `tools/cwl-zen-lint` or implements basic lint checks in Rust. Minimum: validate that the CWL document contains no `InlineJavascriptRequirement`, `ExpressionTool`, `${` blocks, or `when` clauses.

### Moderate Fixes

**5. `inputBinding.separate` not respected (Task 6)**

When `separate: false`, format as `format!("{}{}", prefix, val)` instead of `format!("{} {}", prefix, val)`. Default is true (space-separated).

**6. `inputBinding.shellQuote` not respected (Task 6)**

When `shellQuote: false`, the value should not be quoted when building the command. This matters for shell commands where the value contains pipes/redirects.

**7. `NetworkAccess` handling (Task 8)**

In `run_command()`, when using Docker:
- Default: add `--network=none` for security
- If `NetworkAccess` is in requirements: omit the network flag (allow default Docker networking)

Check with: `tool.requirements.contains_key("NetworkAccess")`

**8. `dateModified` uses wrong timestamp (Task 9)**

`file_entity()` uses `Utc::now()`. Should use:
```rust
let modified = std::fs::metadata(&file.path)?.modified()?;
let dt: DateTime<Utc> = modified.into();
entity["dateModified"] = json!(format_timestamp(&dt));
```

**9. Defaults not applied from workflow/tool definitions**

In `execute_workflow()`, before resolving step inputs, merge workflow-level defaults:
```rust
for (name, wf_input) in &wf.inputs {
    if !inputs.contains_key(name) {
        if let Some(default) = &wf_input.default {
            inputs.insert(name.clone(), yaml_to_resolved(default));
        }
    }
}
```

**10. `file://` URI not stripped**

In `input.rs` `resolve_file_value()`, strip `file://` prefix:
```rust
let path_str = path_str.strip_prefix("file://").unwrap_or(path_str);
```

### Deferred (post-MVP)

These are noted but not critical for the first working version:

- **Singularity support**: Detect `singularity`/`apptainer` on PATH and use it instead of Docker. Same mount semantics, different CLI. Can be added as a `--container-engine` flag.
- **Multiple-caret secondary file patterns**: Only single `^` is handled. Multiple carets (e.g., `^^.bai`) strip multiple extensions. Rare in practice.
- **`Directory` output collection**: Not implemented. Most CWL Zen workflows use File outputs.
- **`outputSource` as array**: Needed for scatter results. Implement when scatter is added.
- **Array input expansion on command line**: Each array element should become a separate argument. Current implementation joins with spaces.
