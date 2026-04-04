use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{
    CommandLineTool, FileValue, GlobPattern, ResolvedValue, RuntimeContext, SecondaryFile,
};
use crate::param;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Collect output files from a working directory using glob patterns.
///
/// For each output declared in `tool.outputs`:
///   - Resolve glob pattern(s) with parameter references
///   - Execute glob against `workdir`
///   - Build `FileValue`s with secondary files
///   - Wrap in `ResolvedValue::Array` (for array types) or single value
///
/// Outputs with `type: stdout` are handled by looking for `tool.stdout` in
/// `workdir`.
pub fn collect_outputs(
    tool: &CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
    workdir: &Path,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut result = HashMap::new();

    for (name, output) in &tool.outputs {
        let base_type = output.cwl_type.base_type();

        // Handle stdout output type
        if base_type == "stdout" {
            let val = collect_stdout_output(tool, workdir)?;
            result.insert(name.clone(), val);
            continue;
        }

        // Handle stderr output type
        if base_type == "stderr" {
            let val = collect_stderr_output(tool, workdir)?;
            result.insert(name.clone(), val);
            continue;
        }

        // Resolve glob patterns
        let patterns = match &output.output_binding {
            Some(binding) => resolve_glob_patterns(&binding.glob, inputs, runtime),
            None => Vec::new(),
        };

        // Execute each pattern against workdir
        let mut matched_files: Vec<ResolvedValue> = Vec::new();
        for pattern in &patterns {
            let full_pattern = workdir.join(pattern);
            let full_pattern_str = full_pattern.to_string_lossy().to_string();
            let entries = glob::glob(&full_pattern_str)
                .with_context(|| format!("invalid glob pattern: {}", full_pattern_str))?;

            for entry in entries {
                let path = entry.with_context(|| "glob iteration error")?;
                let path_str = path.to_string_lossy().to_string();
                let mut fv = FileValue::from_path(&path_str);

                // Collect secondary files
                for sf in &output.secondary_files {
                    let sf_pattern = match sf {
                        SecondaryFile::Pattern(p) => p.clone(),
                        SecondaryFile::Structured(e) => e.pattern.clone(),
                    };
                    let sf_path = resolve_secondary_file_path(&path, &sf_pattern);
                    if sf_path.exists() {
                        let sf_val =
                            FileValue::from_path(sf_path.to_string_lossy().as_ref());
                        fv.secondary_files.push(sf_val);
                    }
                }

                if path.is_dir() {
                    matched_files.push(ResolvedValue::Directory(fv));
                } else {
                    matched_files.push(ResolvedValue::File(fv));
                }
            }
        }

        // Load file contents if loadContents is true
        if let Some(ref binding) = output.output_binding {
            if binding.load_contents.unwrap_or(false) {
                for file_val in &mut matched_files {
                    if let ResolvedValue::File(ref mut fv) = file_val {
                        let path = std::path::Path::new(&fv.path);
                        if path.exists() && path.is_file() {
                            // CWL spec: loadContents reads up to 64 KiB
                            let contents = std::fs::read_to_string(path).unwrap_or_default();
                            let truncated = if contents.len() > 65536 {
                                contents[..65536].to_string()
                            } else {
                                contents
                            };
                            fv.contents = Some(truncated);
                        }
                    }
                }
            }
        }

        // Wrap according to output type
        let mut value = if output.cwl_type.is_array() {
            ResolvedValue::Array(matched_files)
        } else if let Some(first) = matched_files.into_iter().next() {
            first
        } else {
            ResolvedValue::Null
        };

        // Handle outputEval if present
        if let Some(ref binding) = output.output_binding {
            if let Some(ref output_eval) = binding.output_eval {
                // Build `self` for outputEval: an array of the matched file values
                // (CWL spec: self is the array of matched files)
                let self_val = match &value {
                    ResolvedValue::Array(arr) => ResolvedValue::Array(arr.clone()),
                    ResolvedValue::File(_) | ResolvedValue::Directory(_) => {
                        ResolvedValue::Array(vec![value.clone()])
                    }
                    _ => ResolvedValue::Array(vec![]),
                };

                // Try simple parameter reference resolution first
                let eval_str = output_eval.trim();

                // Pattern: $(self[0].contents) or $(parseInt(...)) etc
                if eval_str.starts_with("$(") && eval_str.ends_with(')') {
                    let inner = &eval_str[2..eval_str.len() - 1];
                    if let Some(resolved) =
                        eval_output_eval_param(inner, &self_val, inputs, runtime)
                    {
                        value = resolved;
                    } else if let Some(resolved) =
                        eval_output_eval_dollar_paren(inner, &self_val, inputs, runtime)
                    {
                        value = resolved;
                    }
                }
                // Pattern: ${return parseInt(self[0].contents);} etc
                else if eval_str.starts_with("${") && eval_str.ends_with('}') {
                    if let Some(resolved) =
                        eval_output_eval_js(eval_str, &self_val, inputs, runtime)
                    {
                        value = resolved;
                    }
                }
            }
        }

        // Set format field on File outputs if the output definition has a format
        if let Some(ref fmt) = output.format {
            // Resolve parameter references in the format string
            let resolved_fmt = param::resolve_param_refs(fmt, inputs, runtime, None);
            match &mut value {
                ResolvedValue::File(ref mut fv) => {
                    fv.format = Some(resolved_fmt);
                }
                ResolvedValue::Array(ref mut arr) => {
                    for item in arr.iter_mut() {
                        if let ResolvedValue::File(ref mut fv) = item {
                            fv.format = Some(resolved_fmt.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        result.insert(name.clone(), value);
    }

    Ok(result)
}

/// Resolve a secondary file path from a primary file and a pattern.
///
/// - Pattern `".bai"` -> append to primary path: `/data/sample.bam.bai`
/// - Pattern `"^.bai"` -> replace extension: `/data/sample.bai`
/// - Multiple `^` prefixes strip multiple extensions.
pub fn resolve_secondary_file_path(primary: &Path, pattern: &str) -> PathBuf {
    if let Some(rest) = pattern.strip_prefix('^') {
        // Count leading carets
        let mut stripped = rest;
        let mut path = primary.to_path_buf();

        // The first ^ has already been consumed; strip the extension once.
        path = strip_extension(&path);

        // Each additional ^ strips another extension
        while let Some(after_caret) = stripped.strip_prefix('^') {
            path = strip_extension(&path);
            stripped = after_caret;
        }

        // Append the remaining suffix
        let mut s = path.to_string_lossy().to_string();
        s.push_str(stripped);
        PathBuf::from(s)
    } else {
        // Simple append
        let mut s = primary.to_string_lossy().to_string();
        s.push_str(pattern);
        PathBuf::from(s)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve glob patterns from a `GlobPattern`, expanding any CWL parameter
/// references.
fn resolve_glob_patterns(
    glob: &GlobPattern,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Vec<String> {
    match glob {
        GlobPattern::Single(s) => {
            let resolved = param::resolve_param_refs(s, inputs, runtime, None);
            vec![resolved]
        }
        GlobPattern::Array(patterns) => patterns
            .iter()
            .map(|s| param::resolve_param_refs(s, inputs, runtime, None))
            .collect(),
        GlobPattern::None => Vec::new(),
    }
}

/// Handle `type: stdout` outputs by looking for `tool.stdout` in `workdir`.
fn collect_stdout_output(tool: &CommandLineTool, workdir: &Path) -> Result<ResolvedValue> {
    match &tool.stdout {
        Some(filename) => {
            let stdout_path = workdir.join(filename);
            if stdout_path.exists() {
                let fv = FileValue::from_path(stdout_path.to_string_lossy().as_ref());
                Ok(ResolvedValue::File(fv))
            } else {
                Ok(ResolvedValue::Null)
            }
        }
        None => {
            // CWL spec: if stdout field is absent but type: stdout is used,
            // look for auto-generated *.stdout file in workdir
            find_auto_generated_file(workdir, ".stdout")
        }
    }
}

/// Handle `type: stderr` outputs by looking for `tool.stderr` in `workdir`.
fn collect_stderr_output(tool: &CommandLineTool, workdir: &Path) -> Result<ResolvedValue> {
    match &tool.stderr {
        Some(filename) => {
            let stderr_path = workdir.join(filename);
            if stderr_path.exists() {
                let fv = FileValue::from_path(stderr_path.to_string_lossy().as_ref());
                Ok(ResolvedValue::File(fv))
            } else {
                Ok(ResolvedValue::Null)
            }
        }
        None => {
            // CWL spec: if stderr field is absent but type: stderr is used,
            // look for auto-generated *.stderr file in workdir
            find_auto_generated_file(workdir, ".stderr")
        }
    }
}

/// Find an auto-generated file with the given suffix (e.g. ".stderr", ".stdout")
/// in workdir. These files are created by execute.rs with UUID-based names.
fn find_auto_generated_file(workdir: &Path, suffix: &str) -> Result<ResolvedValue> {
    let pattern = workdir.join(format!("*{}", suffix));
    let pattern_str = pattern.to_string_lossy().to_string();
    if let Ok(entries) = glob::glob(&pattern_str) {
        for entry in entries {
            if let Ok(path) = entry {
                if path.is_file() {
                    let fv = FileValue::from_path(path.to_string_lossy().as_ref());
                    return Ok(ResolvedValue::File(fv));
                }
            }
        }
    }
    Ok(ResolvedValue::Null)
}

/// Evaluate a simple parameter reference in outputEval context.
///
/// Supports patterns like:
///   - `self[0].contents` -> contents of first matched file
///   - `self[0].size` -> size of first matched file
///   - `self[0].path` -> path of first matched file
///   - `self.length` -> number of matched files
///   - `inputs.X` -> input value
fn eval_output_eval_param(
    inner: &str,
    self_val: &ResolvedValue,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Option<ResolvedValue> {
    let inner = inner.trim();

    // self[N].property
    if inner.starts_with("self[") {
        if let Some(bracket_end) = inner.find(']') {
            let idx_str = &inner[5..bracket_end];
            let idx: usize = idx_str.parse().ok()?;
            let rest = &inner[bracket_end + 1..];

            if let ResolvedValue::Array(arr) = self_val {
                let item = arr.get(idx)?;
                if rest.is_empty() {
                    return Some(item.clone());
                }
                let property = rest.strip_prefix('.')?;
                match item {
                    ResolvedValue::File(fv) | ResolvedValue::Directory(fv) => match property {
                        "contents" => {
                            Some(ResolvedValue::String(fv.contents.clone().unwrap_or_default()))
                        }
                        "path" => Some(ResolvedValue::String(fv.path.clone())),
                        "basename" => Some(ResolvedValue::String(fv.basename.clone())),
                        "nameroot" => Some(ResolvedValue::String(fv.nameroot.clone())),
                        "nameext" => Some(ResolvedValue::String(fv.nameext.clone())),
                        "size" => Some(ResolvedValue::Int(fv.size as i64)),
                        _ => None,
                    },
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        }
    } else if inner == "self.length" || inner == "self.length" {
        if let ResolvedValue::Array(arr) = self_val {
            Some(ResolvedValue::Int(arr.len() as i64))
        } else {
            None
        }
    } else if inner == "self" {
        Some(self_val.clone())
    } else if let Some(rest) = inner.strip_prefix("inputs.") {
        // Delegate to param module
        let resolved_str = param::resolve_param_refs(
            &format!("$(inputs.{})", rest),
            inputs,
            runtime,
            Some(self_val),
        );
        // Try to convert back to a typed value
        if resolved_str == "null" {
            Some(ResolvedValue::Null)
        } else if let Ok(n) = resolved_str.parse::<i64>() {
            Some(ResolvedValue::Int(n))
        } else if let Ok(f) = resolved_str.parse::<f64>() {
            Some(ResolvedValue::Float(f))
        } else {
            Some(ResolvedValue::String(resolved_str))
        }
    } else {
        None
    }
}

/// Evaluate a simple JS-like expression in outputEval context.
///
/// Supports patterns like:
///   - `${return parseInt(self[0].contents);}`
///   - `${return parseFloat(self[0].contents);}`
///   - `${return self[0].contents;}`
fn eval_output_eval_js(
    expr: &str,
    self_val: &ResolvedValue,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Option<ResolvedValue> {
    let inner = expr.strip_prefix("${")?.strip_suffix('}')?.trim();
    let body = inner.strip_prefix("return")?.trim();
    let body = body.strip_suffix(';').unwrap_or(body).trim();

    // parseInt(...)
    if let Some(parse_inner) = body
        .strip_prefix("parseInt(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_output_eval_sub_expr(parse_inner.trim(), self_val, inputs, runtime)?;
        let s = match &inner_val {
            ResolvedValue::String(s) => s.clone(),
            ResolvedValue::Int(n) => return Some(ResolvedValue::Int(*n)),
            ResolvedValue::Float(f) => return Some(ResolvedValue::Int(*f as i64)),
            _ => return None,
        };
        // parseInt in JS stops at first non-digit
        let s = s.trim();
        let numeric: String = if s.starts_with('-') {
            std::iter::once('-')
                .chain(s[1..].chars().take_while(|c| c.is_ascii_digit()))
                .collect()
        } else {
            s.chars().take_while(|c| c.is_ascii_digit()).collect()
        };
        let n = numeric.parse::<i64>().ok()?;
        return Some(ResolvedValue::Int(n));
    }

    // parseFloat(...)
    if let Some(parse_inner) = body
        .strip_prefix("parseFloat(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_output_eval_sub_expr(parse_inner.trim(), self_val, inputs, runtime)?;
        let f = match &inner_val {
            ResolvedValue::String(s) => s.trim().parse::<f64>().ok()?,
            ResolvedValue::Int(n) => *n as f64,
            ResolvedValue::Float(f) => *f,
            _ => return None,
        };
        return Some(ResolvedValue::Float(f));
    }

    // Simple expression
    eval_output_eval_sub_expr(body, self_val, inputs, runtime)
}

/// Evaluate an expression inside `$(...)` in outputEval context.
/// Handles `parseInt(self[0].contents)`, `parseFloat(...)`, and other
/// function-call patterns that `eval_output_eval_param` does not cover.
fn eval_output_eval_dollar_paren(
    expr: &str,
    self_val: &ResolvedValue,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Option<ResolvedValue> {
    let expr = expr.trim();

    // parseInt(...)
    if let Some(parse_inner) = expr
        .strip_prefix("parseInt(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_output_eval_sub_expr(parse_inner.trim(), self_val, inputs, runtime)?;
        let s = match &inner_val {
            ResolvedValue::String(s) => s.clone(),
            ResolvedValue::Int(n) => return Some(ResolvedValue::Int(*n)),
            ResolvedValue::Float(f) => return Some(ResolvedValue::Int(*f as i64)),
            _ => return None,
        };
        let s = s.trim();
        let numeric: String = if s.starts_with('-') {
            std::iter::once('-')
                .chain(s[1..].chars().take_while(|c| c.is_ascii_digit()))
                .collect()
        } else {
            s.chars().take_while(|c| c.is_ascii_digit()).collect()
        };
        let n = numeric.parse::<i64>().ok()?;
        return Some(ResolvedValue::Int(n));
    }

    // parseFloat(...)
    if let Some(parse_inner) = expr
        .strip_prefix("parseFloat(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_output_eval_sub_expr(parse_inner.trim(), self_val, inputs, runtime)?;
        let f = match &inner_val {
            ResolvedValue::String(s) => s.trim().parse::<f64>().ok()?,
            ResolvedValue::Int(n) => *n as f64,
            ResolvedValue::Float(f) => *f,
            _ => return None,
        };
        return Some(ResolvedValue::Float(f));
    }

    // Fall through to sub-expr evaluator
    eval_output_eval_sub_expr(expr, self_val, inputs, runtime)
}

/// Evaluate a sub-expression within outputEval JS context.
fn eval_output_eval_sub_expr(
    expr: &str,
    self_val: &ResolvedValue,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Option<ResolvedValue> {
    let expr = expr.trim();

    // self[N].property
    if expr.starts_with("self[") || expr == "self" || expr.starts_with("self.") {
        eval_output_eval_param(expr, self_val, inputs, runtime)
    } else if expr.starts_with("inputs.") {
        eval_output_eval_param(expr, self_val, inputs, runtime)
    } else if let Ok(n) = expr.parse::<i64>() {
        Some(ResolvedValue::Int(n))
    } else if let Ok(f) = expr.parse::<f64>() {
        Some(ResolvedValue::Float(f))
    } else if (expr.starts_with('"') && expr.ends_with('"'))
        || (expr.starts_with('\'') && expr.ends_with('\''))
    {
        Some(ResolvedValue::String(expr[1..expr.len() - 1].to_string()))
    } else {
        None
    }
}

/// Strip the last extension from a path. If no extension, return unchanged.
fn strip_extension(path: &Path) -> PathBuf {
    match path.extension() {
        Some(_) => {
            let mut p = path.to_path_buf();
            p.set_extension("");
            p
        }
        None => path.to_path_buf(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileValue, GlobPattern, ResolvedValue, RuntimeContext};

    fn test_inputs() -> HashMap<String, ResolvedValue> {
        let mut inputs = HashMap::new();
        inputs.insert(
            "sample_id".to_string(),
            ResolvedValue::String("SRX123".to_string()),
        );
        inputs.insert(
            "bam".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/sample.sorted.bam".to_string(),
                basename: "sample.sorted.bam".to_string(),
                nameroot: "sample.sorted".to_string(),
                nameext: ".bam".to_string(),
                size: 1024,
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
                format: None,
            }),
        );
        inputs
    }

    fn test_runtime() -> RuntimeContext {
        RuntimeContext {
            cores: 4,
            ram: 8192,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        }
    }

    // -- resolve_secondary_file_path -------------------------------------------

    #[test]
    fn secondary_file_suffix() {
        let primary = Path::new("/data/sample.bam");
        let result = resolve_secondary_file_path(primary, ".bai");
        assert_eq!(result, PathBuf::from("/data/sample.bam.bai"));
    }

    #[test]
    fn secondary_file_caret() {
        let primary = Path::new("/data/sample.bam");
        let result = resolve_secondary_file_path(primary, "^.bai");
        assert_eq!(result, PathBuf::from("/data/sample.bai"));
    }

    // -- resolve_glob_patterns (helper) ----------------------------------------

    #[test]
    fn resolve_glob_single() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let glob = GlobPattern::Single("*.bam".to_string());
        let result = resolve_glob_patterns(&glob, &inputs, &runtime);
        assert_eq!(result, vec!["*.bam"]);
    }

    #[test]
    fn resolve_glob_with_param() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let glob = GlobPattern::Single("$(inputs.sample_id).sorted.bam".to_string());
        let result = resolve_glob_patterns(&glob, &inputs, &runtime);
        assert_eq!(result, vec!["SRX123.sorted.bam"]);
    }

    #[test]
    fn resolve_glob_array() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let glob = GlobPattern::Array(vec!["*.bam".to_string(), "*.cram".to_string()]);
        let result = resolve_glob_patterns(&glob, &inputs, &runtime);
        assert_eq!(result, vec!["*.bam", "*.cram"]);
    }

    // -- collect_outputs with real files on disk --------------------------------

    #[test]
    fn collect_outputs_with_glob() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create test files
        fs::write(workdir.join("result.txt"), "hello").unwrap();
        fs::write(workdir.join("other.log"), "log data").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "outfile".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("File".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("*.txt".to_string()),
                    load_contents: None,
                    output_eval: None,
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("outfile").unwrap() {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "result.txt");
                assert!(fv.path.ends_with("result.txt"));
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn collect_outputs_stderr_with_explicit_filename() {
        use crate::model::{CommandLineTool, CwlType, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create stderr file
        fs::write(workdir.join("std.err"), "error output").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "output_file".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("stderr".to_string()),
                output_binding: None,
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: Some("std.err".to_string()),
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("output_file").unwrap() {
            ResolvedValue::File(fv) => {
                assert!(fv.path.ends_with("std.err"));
                assert_eq!(fv.basename, "std.err");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn collect_outputs_stderr_auto_generated() {
        use crate::model::{CommandLineTool, CwlType, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create an auto-generated stderr file (like execute.rs would)
        let auto_name = format!("{}.stderr", uuid::Uuid::new_v4());
        fs::write(workdir.join(&auto_name), "error output").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "output_file".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("stderr".to_string()),
                output_binding: None,
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None, // No explicit stderr field
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("output_file").unwrap() {
            ResolvedValue::File(fv) => {
                assert!(fv.basename.ends_with(".stderr"));
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    // -- loadContents tests --------------------------------------------------

    #[test]
    fn collect_outputs_load_contents() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create test file with known contents
        fs::write(workdir.join("count.txt"), "  42\n").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "outfile".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("File".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("count.txt".to_string()),
                    load_contents: Some(true),
                    output_eval: None,
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("outfile").unwrap() {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "count.txt");
                assert!(fv.contents.is_some());
                assert_eq!(fv.contents.as_ref().unwrap(), "  42\n");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn collect_outputs_load_contents_false() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        fs::write(workdir.join("data.txt"), "some data").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "outfile".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("File".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("data.txt".to_string()),
                    load_contents: Some(false),
                    output_eval: None,
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("outfile").unwrap() {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "data.txt");
                assert!(fv.contents.is_none());
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn collect_outputs_output_eval_parse_int() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        fs::write(workdir.join("count.txt"), "  16\n").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "count".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("int".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("count.txt".to_string()),
                    load_contents: Some(true),
                    output_eval: Some("${return parseInt(self[0].contents);}".to_string()),
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("wc".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("count").unwrap() {
            ResolvedValue::Int(n) => assert_eq!(*n, 16),
            other => panic!("expected Int(16), got {:?}", other),
        }
    }

    // -- outputEval with parameter reference ----------------------------------

    #[test]
    fn collect_outputs_output_eval_self_contents() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        fs::write(workdir.join("result.txt"), "hello world").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "text".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("string".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("result.txt".to_string()),
                    load_contents: Some(true),
                    output_eval: Some("$(self[0].contents)".to_string()),
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("echo".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("text").unwrap() {
            ResolvedValue::String(s) => assert_eq!(s, "hello world"),
            other => panic!("expected String(\"hello world\"), got {:?}", other),
        }
    }

    // -- outputEval with $(parseInt(...)) pattern (dollar-paren) ---------------

    #[test]
    fn collect_outputs_output_eval_dollar_paren_parse_int() {
        use crate::model::{CommandLineTool, CwlType, OutputBinding, ToolOutput};
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        fs::write(workdir.join("output.txt"), "  16  198 1111 /tmp/whale.txt\n").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert(
            "output".to_string(),
            ToolOutput {
                id: None,
                cwl_type: CwlType::Single("int".to_string()),
                output_binding: Some(OutputBinding {
                    glob: GlobPattern::Single("output.txt".to_string()),
                    load_contents: Some(true),
                    output_eval: Some("$(parseInt(self[0].contents))".to_string()),
                }),
                secondary_files: Vec::new(),
                doc: None,
                format: None,
            },
        );

        let tool = CommandLineTool {
            cwl_version: None,
            label: None,
            doc: None,
            base_command: crate::model::BaseCommand::Single("wc".to_string()),
            arguments: Vec::new(),
            inputs: HashMap::new(),
            outputs,
            requirements: Vec::new(),
            hints: Vec::new(),
            stdout: None,
            stdin: None,
            stderr: None,
        };

        let inputs = HashMap::new();
        let runtime = test_runtime();
        let result = collect_outputs(&tool, &inputs, &runtime, workdir).unwrap();

        match result.get("output").unwrap() {
            ResolvedValue::Int(n) => assert_eq!(*n, 16),
            other => panic!("expected Int(16), got {:?}", other),
        }
    }
}
