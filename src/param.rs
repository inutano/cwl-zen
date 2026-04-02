use std::collections::HashMap;

use crate::model::{FileValue, ResolvedValue, RuntimeContext};

/// Resolve all `$(...)` parameter references in a string.
///
/// CWL parameter references: `$(inputs.X)`, `$(inputs.X.path)`,
/// `$(runtime.cores)`, `$(self)`.
///
/// Escaped `\$(...)` is left as `$(...)` for shell command substitution.
/// Escaped `\$VAR` is left as `$VAR` for shell variables.
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
        if chars[i] == '\\' && i + 1 < len && chars[i + 1] == '$' {
            // Escaped dollar: emit '$' and skip the backslash
            result.push('$');
            i += 2;
        } else if chars[i] == '$' && i + 1 < len && chars[i + 1] == '(' {
            // CWL parameter reference: find matching ')'
            let start = i + 2; // after "$("
            if let Some(end) = find_closing_paren(&chars, start) {
                let expr: String = chars[start..end].iter().collect();
                let resolved = resolve_expression(&expr, inputs, runtime, self_val);
                result.push_str(&resolved);
                i = end + 1; // skip past ')'
            } else {
                // No closing paren — emit literally
                result.push('$');
                result.push('(');
                i += 2;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Convert a `ResolvedValue` to its string representation.
pub fn value_to_string(val: &ResolvedValue) -> String {
    match val {
        ResolvedValue::String(s) => s.clone(),
        ResolvedValue::Int(n) => n.to_string(),
        ResolvedValue::Float(f) => f.to_string(),
        ResolvedValue::Bool(b) => b.to_string(),
        ResolvedValue::File(fv) => fv.path.clone(),
        ResolvedValue::Directory(fv) => fv.path.clone(),
        ResolvedValue::Array(arr) => arr.iter().map(value_to_string).collect::<Vec<_>>().join(" "),
        ResolvedValue::Null => "null".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Find the index of the closing ')' matching an opening '(' that was already
/// consumed. `start` points to the first character after '('. Returns the index
/// of ')' or `None`.
fn find_closing_paren(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 1u32;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Resolve a single expression (the text between `$(` and `)`).
fn resolve_expression(
    expr: &str,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
    self_val: Option<&ResolvedValue>,
) -> String {
    if let Some(rest) = expr.strip_prefix("inputs.") {
        resolve_input_expr(rest, inputs)
    } else if let Some(rest) = expr.strip_prefix("runtime.") {
        resolve_runtime_expr(rest, runtime)
    } else if expr == "self" {
        match self_val {
            Some(v) => value_to_string(v),
            None => "null".to_string(),
        }
    } else if let Some(rest) = expr.strip_prefix("self.") {
        resolve_self_property(rest, self_val)
    } else {
        // Unknown expression — return "null"
        "null".to_string()
    }
}

/// Resolve `inputs.X` or `inputs.X.property`.
fn resolve_input_expr(rest: &str, inputs: &HashMap<String, ResolvedValue>) -> String {
    // Split on the first '.' to get <name> and optional <property>
    if let Some(dot_pos) = rest.find('.') {
        let name = &rest[..dot_pos];
        let property = &rest[dot_pos + 1..];
        match inputs.get(name) {
            Some(val) => resolve_property(val, property),
            None => "null".to_string(),
        }
    } else {
        // No property — just the input value
        match inputs.get(rest) {
            Some(val) => value_to_string(val),
            None => "null".to_string(),
        }
    }
}

/// Resolve a property access on a resolved value (e.g., `.path`, `.basename`).
fn resolve_property(val: &ResolvedValue, property: &str) -> String {
    match val {
        ResolvedValue::File(fv) | ResolvedValue::Directory(fv) => {
            resolve_file_property(fv, property)
        }
        _ => "null".to_string(),
    }
}

/// Resolve a file/directory property.
fn resolve_file_property(fv: &FileValue, property: &str) -> String {
    match property {
        "path" => fv.path.clone(),
        "basename" => fv.basename.clone(),
        "nameroot" => fv.nameroot.clone(),
        "nameext" => fv.nameext.clone(),
        "size" => fv.size.to_string(),
        _ => "null".to_string(),
    }
}

/// Resolve `runtime.<field>`.
fn resolve_runtime_expr(field: &str, runtime: &RuntimeContext) -> String {
    match field {
        "cores" => runtime.cores.to_string(),
        "ram" => runtime.ram.to_string(),
        "outdir" => runtime.outdir.clone(),
        "tmpdir" => runtime.tmpdir.clone(),
        _ => "null".to_string(),
    }
}

/// Resolve `self.<property>`.
fn resolve_self_property(property: &str, self_val: Option<&ResolvedValue>) -> String {
    match self_val {
        Some(val) => resolve_property(val, property),
        None => "null".to_string(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileValue, ResolvedValue, RuntimeContext};

    /// Build a test input map with sample_id (String), count (Int), and bam (File).
    fn test_inputs() -> HashMap<String, ResolvedValue> {
        let mut inputs = HashMap::new();
        inputs.insert(
            "sample_id".to_string(),
            ResolvedValue::String("SRX123".to_string()),
        );
        inputs.insert("count".to_string(), ResolvedValue::Int(42));
        inputs.insert(
            "bam".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/sample.sorted.bam".to_string(),
                basename: "sample.sorted.bam".to_string(),
                nameroot: "sample.sorted".to_string(),
                nameext: ".bam".to_string(),
                size: 1024,
                secondary_files: Vec::new(),
            }),
        );
        inputs
    }

    /// Build a test runtime context.
    fn test_runtime() -> RuntimeContext {
        RuntimeContext {
            cores: 4,
            ram: 8192,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        }
    }

    #[test]
    fn resolve_simple_input() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let result = resolve_param_refs("$(inputs.sample_id)", &inputs, &runtime, None);
        assert_eq!(result, "SRX123");
    }

    #[test]
    fn resolve_string_interpolation() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let result =
            resolve_param_refs("$(inputs.sample_id).sorted.bam", &inputs, &runtime, None);
        assert_eq!(result, "SRX123.sorted.bam");
    }

    #[test]
    fn resolve_file_properties() {
        let inputs = test_inputs();
        let runtime = test_runtime();

        assert_eq!(
            resolve_param_refs("$(inputs.bam.path)", &inputs, &runtime, None),
            "/data/sample.sorted.bam"
        );
        assert_eq!(
            resolve_param_refs("$(inputs.bam.basename)", &inputs, &runtime, None),
            "sample.sorted.bam"
        );
        assert_eq!(
            resolve_param_refs("$(inputs.bam.nameroot)", &inputs, &runtime, None),
            "sample.sorted"
        );
        assert_eq!(
            resolve_param_refs("$(inputs.bam.nameext)", &inputs, &runtime, None),
            ".bam"
        );
        assert_eq!(
            resolve_param_refs("$(inputs.bam.size)", &inputs, &runtime, None),
            "1024"
        );
    }

    #[test]
    fn resolve_runtime() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        assert_eq!(
            resolve_param_refs("$(runtime.cores)", &inputs, &runtime, None),
            "4"
        );
        assert_eq!(
            resolve_param_refs("$(runtime.ram)", &inputs, &runtime, None),
            "8192"
        );
        assert_eq!(
            resolve_param_refs("$(runtime.outdir)", &inputs, &runtime, None),
            "/tmp/out"
        );
        assert_eq!(
            resolve_param_refs("$(runtime.tmpdir)", &inputs, &runtime, None),
            "/tmp/tmp"
        );
    }

    #[test]
    fn resolve_self() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let self_val = ResolvedValue::String("SRX123".to_string());
        let result = resolve_param_refs("$(self).05", &inputs, &runtime, Some(&self_val));
        assert_eq!(result, "SRX123.05");
    }

    #[test]
    fn resolve_self_path() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let self_val = ResolvedValue::File(FileValue {
            path: "/data/input.bam".to_string(),
            basename: "input.bam".to_string(),
            nameroot: "input".to_string(),
            nameext: ".bam".to_string(),
            size: 512,
            secondary_files: Vec::new(),
        });
        let result = resolve_param_refs("$(self.path)", &inputs, &runtime, Some(&self_val));
        assert_eq!(result, "/data/input.bam");
    }

    #[test]
    fn resolve_escaped_dollar() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let result = resolve_param_refs(
            "\\$(cat $(inputs.bam.path))",
            &inputs,
            &runtime,
            None,
        );
        assert_eq!(result, "$(cat /data/sample.sorted.bam)");
    }

    #[test]
    fn resolve_mixed_cwl_and_shell() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        // Mix of CWL refs and escaped shell vars
        let template = "samtools view -@ $(runtime.cores) \\$INPUT > $(inputs.sample_id).bam";
        let result = resolve_param_refs(template, &inputs, &runtime, None);
        assert_eq!(result, "samtools view -@ 4 $INPUT > SRX123.bam");
    }

    #[test]
    fn resolve_rg_header() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let template = "@RG\\tID:$(inputs.sample_id)\\tSM:$(inputs.sample_id)";
        let result = resolve_param_refs(template, &inputs, &runtime, None);
        assert_eq!(result, "@RG\\tID:SRX123\\tSM:SRX123");
    }

    #[test]
    fn resolve_null_input() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let result = resolve_param_refs("$(inputs.missing)", &inputs, &runtime, None);
        assert_eq!(result, "null");
    }

    #[test]
    fn resolve_int_input() {
        let inputs = test_inputs();
        let runtime = test_runtime();
        let result = resolve_param_refs("$(inputs.count)", &inputs, &runtime, None);
        assert_eq!(result, "42");
    }

    #[test]
    fn value_to_string_variants() {
        assert_eq!(
            value_to_string(&ResolvedValue::String("hello".to_string())),
            "hello"
        );
        assert_eq!(value_to_string(&ResolvedValue::Int(99)), "99");
        assert_eq!(value_to_string(&ResolvedValue::Float(3.14)), "3.14");
        assert_eq!(value_to_string(&ResolvedValue::Bool(true)), "true");
        assert_eq!(value_to_string(&ResolvedValue::Null), "null");

        let fv = FileValue {
            path: "/data/file.txt".to_string(),
            basename: "file.txt".to_string(),
            nameroot: "file".to_string(),
            nameext: ".txt".to_string(),
            size: 0,
            secondary_files: Vec::new(),
        };
        assert_eq!(
            value_to_string(&ResolvedValue::File(fv.clone())),
            "/data/file.txt"
        );
        assert_eq!(
            value_to_string(&ResolvedValue::Directory(fv)),
            "/data/file.txt"
        );

        let arr = ResolvedValue::Array(vec![
            ResolvedValue::String("a".to_string()),
            ResolvedValue::String("b".to_string()),
            ResolvedValue::Int(3),
        ]);
        assert_eq!(value_to_string(&arr), "a b 3");
    }
}
