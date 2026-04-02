use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{FileValue, ResolvedValue};

// ---------------------------------------------------------------------------
// Staging mode
// ---------------------------------------------------------------------------

/// How input files are placed into the working directory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StagingMode {
    /// Create symbolic links (fast, default).
    Symlink,
    /// Create full copies (for tools that cannot handle symlinks).
    Copy,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Stage input files into `workdir` by creating symlinks or copies.
///
/// For each input:
/// - `ResolvedValue::File` / `ResolvedValue::Directory`: stage the primary
///   path and all secondary files, returning updated `FileValue`s whose
///   `path` fields point into `workdir`.
/// - `ResolvedValue::Array`: recursively stage each element.
/// - Other types: pass through unchanged.
pub fn stage_inputs(
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    mode: StagingMode,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut staged = HashMap::new();
    for (name, value) in inputs {
        let new_value = stage_value(value, workdir, mode)?;
        staged.insert(name.clone(), new_value);
    }
    Ok(staged)
}

/// Returns `true` if `stderr_content` contains patterns that suggest the
/// command failed because of symlink-related issues (read-only filesystem,
/// too many symlink levels, permission errors).
pub fn is_symlink_error(stderr_content: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "Read-only file system",
        "Too many levels of symbolic links",
        "Operation not permitted",
        "Permission denied",
    ];
    PATTERNS
        .iter()
        .any(|pat| stderr_content.contains(pat))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Recursively stage a single `ResolvedValue`.
fn stage_value(
    value: &ResolvedValue,
    workdir: &Path,
    mode: StagingMode,
) -> Result<ResolvedValue> {
    match value {
        ResolvedValue::File(fv) => {
            let staged_fv = stage_file_value(fv, workdir, mode)?;
            Ok(ResolvedValue::File(staged_fv))
        }
        ResolvedValue::Directory(fv) => {
            let staged_fv = stage_file_value(fv, workdir, mode)?;
            Ok(ResolvedValue::Directory(staged_fv))
        }
        ResolvedValue::Array(arr) => {
            let staged_arr: Result<Vec<ResolvedValue>> = arr
                .iter()
                .map(|v| stage_value(v, workdir, mode))
                .collect();
            Ok(ResolvedValue::Array(staged_arr?))
        }
        // Pass through unchanged
        other => Ok(other.clone()),
    }
}

/// Stage a single `FileValue` (primary path + secondary files) into `workdir`.
fn stage_file_value(
    fv: &FileValue,
    workdir: &Path,
    mode: StagingMode,
) -> Result<FileValue> {
    let dest = workdir.join(&fv.basename);
    stage_one(&fv.path, &dest, mode)?;

    // Stage secondary files
    let mut staged_secondaries = Vec::new();
    for sf in &fv.secondary_files {
        let sf_dest = workdir.join(&sf.basename);
        stage_one(&sf.path, &sf_dest, mode)?;
        let sf_dest_str = sf_dest.to_string_lossy().to_string();
        staged_secondaries.push(FileValue::from_path(&sf_dest_str));
    }

    let dest_str = dest.to_string_lossy().to_string();
    let mut new_fv = FileValue::from_path(&dest_str);
    new_fv.secondary_files = staged_secondaries;
    Ok(new_fv)
}

/// Create a single symlink or copy from `src` to `dest`.
///
/// Skips if the source does not exist or the destination already exists.
fn stage_one(src: &str, dest: &Path, mode: StagingMode) -> Result<()> {
    let src_path = Path::new(src);

    // Skip if source doesn't exist or destination already exists
    if !src_path.exists() || dest.exists() {
        return Ok(());
    }

    match mode {
        StagingMode::Symlink => {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(src_path, dest).with_context(|| {
                    format!(
                        "failed to symlink {} -> {}",
                        src_path.display(),
                        dest.display()
                    )
                })?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(src_path, dest).with_context(|| {
                    format!(
                        "failed to copy {} -> {} (symlinks not supported on this platform)",
                        src_path.display(),
                        dest.display()
                    )
                })?;
            }
        }
        StagingMode::Copy => {
            std::fs::copy(src_path, dest).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    src_path.display(),
                    dest.display()
                )
            })?;
        }
    }

    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileValue;
    use std::collections::HashMap;

    /// Create a temp file, stage with Symlink mode, verify symlink exists and
    /// staged inputs point to workdir.
    #[test]
    fn symlink_staging_creates_links() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = tempfile::tempdir().unwrap();

        // Create a source file
        let src_path = dir.path().join("reads.fastq");
        std::fs::write(&src_path, "ACGT").unwrap();

        let mut inputs = HashMap::new();
        inputs.insert(
            "infile".to_string(),
            ResolvedValue::File(FileValue::from_path(
                src_path.to_string_lossy().as_ref(),
            )),
        );

        let staged = stage_inputs(&inputs, workdir.path(), StagingMode::Symlink).unwrap();

        // Verify the staged value points into workdir
        match staged.get("infile").unwrap() {
            ResolvedValue::File(fv) => {
                assert!(
                    fv.path.starts_with(workdir.path().to_string_lossy().as_ref()),
                    "staged path {} should be under workdir {}",
                    fv.path,
                    workdir.path().display()
                );
                assert_eq!(fv.basename, "reads.fastq");

                // Verify it's actually a symlink
                let staged_path = Path::new(&fv.path);
                assert!(
                    staged_path.symlink_metadata().unwrap().file_type().is_symlink(),
                    "staged file should be a symlink"
                );

                // Verify content is correct (readable through symlink)
                let content = std::fs::read_to_string(staged_path).unwrap();
                assert_eq!(content, "ACGT");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    /// Create a temp file, stage with Copy mode, verify it's a real file (not
    /// symlink) with correct content.
    #[test]
    fn copy_staging_creates_copies() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = tempfile::tempdir().unwrap();

        // Create a source file
        let src_path = dir.path().join("data.csv");
        std::fs::write(&src_path, "a,b,c\n1,2,3").unwrap();

        let mut inputs = HashMap::new();
        inputs.insert(
            "infile".to_string(),
            ResolvedValue::File(FileValue::from_path(
                src_path.to_string_lossy().as_ref(),
            )),
        );

        let staged = stage_inputs(&inputs, workdir.path(), StagingMode::Copy).unwrap();

        match staged.get("infile").unwrap() {
            ResolvedValue::File(fv) => {
                assert!(
                    fv.path.starts_with(workdir.path().to_string_lossy().as_ref()),
                    "staged path {} should be under workdir {}",
                    fv.path,
                    workdir.path().display()
                );
                assert_eq!(fv.basename, "data.csv");

                // Verify it's a real file, not a symlink
                let staged_path = Path::new(&fv.path);
                assert!(
                    !staged_path.symlink_metadata().unwrap().file_type().is_symlink(),
                    "staged file should NOT be a symlink"
                );

                // Verify content is correct
                let content = std::fs::read_to_string(staged_path).unwrap();
                assert_eq!(content, "a,b,c\n1,2,3");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    /// Verify each symlink error pattern is detected, and normal errors are not.
    #[test]
    fn is_symlink_error_detected() {
        // These should be detected
        assert!(is_symlink_error("Read-only file system"));
        assert!(is_symlink_error("error: Too many levels of symbolic links"));
        assert!(is_symlink_error("Operation not permitted on /foo/bar"));
        assert!(is_symlink_error("Permission denied"));
        assert!(is_symlink_error("some prefix: Read-only file system: /dev/null"));

        // These should NOT be detected
        assert!(!is_symlink_error("No such file or directory"));
        assert!(!is_symlink_error("command not found"));
        assert!(!is_symlink_error("segmentation fault"));
        assert!(!is_symlink_error(""));
    }
}
