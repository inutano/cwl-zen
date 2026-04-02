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

        // Wrap according to output type
        let value = if output.cwl_type.is_array() {
            ResolvedValue::Array(matched_files)
        } else if let Some(first) = matched_files.into_iter().next() {
            first
        } else {
            ResolvedValue::Null
        };

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
        None => Ok(ResolvedValue::Null),
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
                }),
                secondary_files: Vec::new(),
                doc: None,
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
}
