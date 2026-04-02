use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{FileValue, ResolvedValue};

/// Parse a CWL input YAML file into a map of resolved values.
///
/// Each top-level key becomes an entry in the returned `HashMap`. Values are
/// converted to `ResolvedValue` according to their YAML type, with special
/// handling for `class: File` and `class: Directory` mappings.
///
/// Relative file paths in `File` / `Directory` entries are resolved against
/// `base_dir`.
pub fn parse_inputs(path: &Path, base_dir: &Path) -> Result<HashMap<String, ResolvedValue>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let top: serde_yaml::Value =
        serde_yaml::from_str(&content).context("failed to parse input YAML")?;

    let mapping = top
        .as_mapping()
        .context("input YAML must be a top-level mapping")?;

    let mut result = HashMap::new();
    for (key, value) in mapping {
        let key_str = key
            .as_str()
            .context("input key must be a string")?
            .to_string();
        let resolved = yaml_to_resolved(value, base_dir);
        result.insert(key_str, resolved);
    }
    Ok(result)
}

/// Recursively convert a `serde_yaml::Value` into a `ResolvedValue`.
fn yaml_to_resolved(val: &serde_yaml::Value, base_dir: &Path) -> ResolvedValue {
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
            let items = seq.iter().map(|v| yaml_to_resolved(v, base_dir)).collect();
            ResolvedValue::Array(items)
        }
        serde_yaml::Value::Mapping(map) => {
            // Check for class: File or class: Directory
            let class = map
                .get(serde_yaml::Value::String("class".to_string()))
                .and_then(|v| v.as_str());

            match class {
                Some("File") => resolve_file_mapping(map, base_dir),
                Some("Directory") => resolve_directory_mapping(map, base_dir),
                _ => {
                    // Generic mapping — not a File/Directory, treat as null
                    // (CWL inputs are typically scalar, File, Directory, or array)
                    ResolvedValue::Null
                }
            }
        }
        // Tagged values — treat as null
        _ => ResolvedValue::Null,
    }
}

/// Resolve a YAML mapping with `class: File` into `ResolvedValue::File`.
fn resolve_file_mapping(
    map: &serde_yaml::Mapping,
    base_dir: &Path,
) -> ResolvedValue {
    let raw_path = map
        .get(serde_yaml::Value::String("path".to_string()))
        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let resolved_path = resolve_file_path(raw_path, base_dir);
    let mut fv = FileValue::from_path(&resolved_path);

    // Process secondaryFiles if present
    if let Some(serde_yaml::Value::Sequence(seq)) =
        map.get(serde_yaml::Value::String("secondaryFiles".to_string()))
    {
        for entry in seq {
            if let serde_yaml::Value::Mapping(sf_map) = entry {
                let sf_class = sf_map
                    .get(serde_yaml::Value::String("class".to_string()))
                    .and_then(|v| v.as_str());
                if sf_class == Some("File") || sf_class == Some("Directory") {
                    let sf_raw = sf_map
                        .get(serde_yaml::Value::String("path".to_string()))
                        .or_else(|| {
                            sf_map.get(serde_yaml::Value::String("location".to_string()))
                        })
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let sf_resolved = resolve_file_path(sf_raw, base_dir);
                    fv.secondary_files.push(FileValue::from_path(&sf_resolved));
                }
            }
        }
    }
    ResolvedValue::File(fv)
}

/// Resolve a YAML mapping with `class: Directory` into `ResolvedValue::Directory`.
fn resolve_directory_mapping(
    map: &serde_yaml::Mapping,
    base_dir: &Path,
) -> ResolvedValue {
    let raw_path = map
        .get(serde_yaml::Value::String("path".to_string()))
        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let resolved_path = resolve_file_path(raw_path, base_dir);
    let fv = FileValue::from_path(&resolved_path);
    ResolvedValue::Directory(fv)
}

/// Clean up and resolve a file path:
/// - Strip `file://` prefix if present
/// - Resolve relative paths against `base_dir`
fn resolve_file_path(raw: &str, base_dir: &Path) -> String {
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    let p = Path::new(stripped);
    if p.is_absolute() {
        stripped.to_string()
    } else {
        base_dir.join(stripped).to_string_lossy().to_string()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn base_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn parse_echo_input() {
        let inputs = parse_inputs(&fixture_path("echo-input.yml"), &base_dir()).unwrap();
        match inputs.get("message") {
            Some(ResolvedValue::String(s)) => {
                assert_eq!(s, "Hello, CWL Zen!");
            }
            other => panic!("expected String(\"Hello, CWL Zen!\"), got {:?}", other),
        }
    }

    #[test]
    fn parse_cat_input() {
        let inputs = parse_inputs(&fixture_path("cat-input.yml"), &base_dir()).unwrap();
        match inputs.get("input_file") {
            Some(ResolvedValue::File(fv)) => {
                let expected = base_dir()
                    .join("tests/fixtures/echo-input.yml")
                    .to_string_lossy()
                    .to_string();
                assert_eq!(fv.path, expected);
                assert_eq!(fv.basename, "echo-input.yml");
                assert_eq!(fv.nameroot, "echo-input");
                assert_eq!(fv.nameext, ".yml");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn parse_two_step_input() {
        let inputs = parse_inputs(&fixture_path("two-step-input.yml"), &base_dir()).unwrap();
        match inputs.get("message") {
            Some(ResolvedValue::String(s)) => {
                assert_eq!(s, "Hello from two-step");
            }
            other => panic!("expected String, got {:?}", other),
        }
        match inputs.get("prefix") {
            Some(ResolvedValue::String(s)) => {
                assert_eq!(s, "ZEN");
            }
            other => panic!("expected String, got {:?}", other),
        }
    }
}
