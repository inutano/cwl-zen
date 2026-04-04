use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{CommandLineTool, CwlDocument};

/// Parse a CWL document from a file path.
///
/// Supports `$graph` packed format with fragment identifiers, e.g.
/// `tests/scatter-wf3.cwl#main` — extracts the entry with matching `id`.
pub fn parse_cwl(path: &Path) -> Result<CwlDocument> {
    let path_str = path.to_string_lossy();

    // Check for fragment identifier (e.g. "file.cwl#main")
    let (file_path, fragment) = if let Some(hash_pos) = path_str.find('#') {
        let fp = Path::new(&path_str[..hash_pos]);
        let frag = &path_str[hash_pos + 1..];
        (fp.to_path_buf(), Some(frag.to_string()))
    } else {
        (path.to_path_buf(), None)
    };

    let content = std::fs::read_to_string(&file_path)
        .with_context(|| format!("reading {}", file_path.display()))?;

    if let Some(ref frag) = fragment {
        // Try to parse as $graph packed format
        if let Ok(doc) = parse_cwl_graph(&content, frag) {
            return Ok(doc);
        }
    }

    parse_cwl_str(&content)
}

/// Parse a `$graph` packed CWL document and extract the entry matching `fragment_id`.
///
/// Resolves `run: "#id"` references in workflow steps by inlining the
/// referenced tool definition from the $graph.
fn parse_cwl_graph(content: &str, fragment_id: &str) -> Result<CwlDocument> {
    let content = if content.starts_with("#!") {
        match content.find('\n') {
            Some(pos) => &content[pos + 1..],
            None => "",
        }
    } else {
        content
    };

    let raw: serde_yaml::Value =
        serde_yaml::from_str(content).context("failed to parse CWL document")?;

    // Extract $namespaces
    let namespaces = extract_namespaces(content);

    let graph = raw
        .get("$graph")
        .and_then(|v| v.as_sequence())
        .with_context(|| "no $graph field in packed CWL document")?;

    // Build a map of id -> entry for resolving references.
    // IDs may or may not have "#" prefix depending on the packing tool.
    let mut entry_map: std::collections::HashMap<String, &serde_yaml::Value> =
        std::collections::HashMap::new();
    for entry in graph {
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            // Store under the raw ID
            entry_map.insert(id.to_string(), entry);
            // Also store without "#" prefix for matching
            let bare_id = id.strip_prefix('#').unwrap_or(id);
            entry_map.insert(bare_id.to_string(), entry);
            // And with "#" prefix
            if !id.starts_with('#') {
                entry_map.insert(format!("#{}", id), entry);
            }
        }
    }

    let target = entry_map
        .get(fragment_id)
        .with_context(|| format!("fragment '{}' not found in $graph", fragment_id))?;

    // Pre-process the entry to strip packed-format ID prefixes.
    // Packed CWL uses IDs like "#main/input" which need to be shortened
    // to just "input" for our parser to work correctly.
    let cleaned_entry = strip_packed_prefixes((*target).clone());

    let mut doc: CwlDocument = serde_yaml::from_value(cleaned_entry)
        .with_context(|| format!("parsing $graph entry '{}'", fragment_id))?;

    // Resolve `run: "#id"` references in workflow steps by inlining
    if let CwlDocument::Workflow(ref mut wf) = doc {
        for step in wf.steps.values_mut() {
            if let crate::model::StepRun::Path(ref run_ref) = step.run {
                if run_ref.starts_with('#') {
                    let ref_id = run_ref.strip_prefix('#').unwrap_or(run_ref);
                    if let Some(tool_entry) = entry_map.get(ref_id)
                        .or_else(|| entry_map.get(run_ref.as_str()))
                    {
                        // Strip packed prefixes from the tool entry too
                        let cleaned_tool = strip_packed_prefixes((*tool_entry).clone());
                        if let Ok(tool_doc) = serde_yaml::from_value::<CwlDocument>(cleaned_tool) {
                            step.run = crate::model::StepRun::Inline(Box::new(tool_doc));
                        }
                    }
                }
            }
        }
    }

    if !namespaces.is_empty() {
        resolve_format_namespaces(&mut doc, &namespaces);
    }

    Ok(doc)
}

/// Strip packed-format ID prefixes from a YAML value.
///
/// In CWL packed format, IDs use fully-qualified names like `#main/input`,
/// `#main/step1/output`. This function strips these to bare names.
fn strip_packed_prefixes(val: serde_yaml::Value) -> serde_yaml::Value {
    match val {
        serde_yaml::Value::Mapping(mut map) => {
            // Strip `id` fields: use last segment (e.g. "#main/rev/input" -> "input")
            let id_key = serde_yaml::Value::String("id".to_string());
            if let Some(serde_yaml::Value::String(s)) = map.get(&id_key).cloned() {
                map.insert(id_key, serde_yaml::Value::String(strip_packed_id_last(&s)));
            }

            // Strip `source` and `outputSource`: strip first segment only
            // "#main/rev/output" -> "rev/output" (step/output reference)
            for field in &["source", "outputSource"] {
                let key = serde_yaml::Value::String(field.to_string());
                if let Some(v) = map.get(&key).cloned() {
                    match v {
                        serde_yaml::Value::String(s) => {
                            map.insert(key, serde_yaml::Value::String(strip_packed_id_first(&s)));
                        }
                        serde_yaml::Value::Sequence(seq) => {
                            let stripped: Vec<serde_yaml::Value> = seq.into_iter().map(|item| {
                                if let serde_yaml::Value::String(s) = item {
                                    serde_yaml::Value::String(strip_packed_id_first(&s))
                                } else {
                                    item
                                }
                            }).collect();
                            map.insert(key, serde_yaml::Value::Sequence(stripped));
                        }
                        _ => {}
                    }
                }
            }

            // Strip `out` list entries (step outputs): use last segment
            let out_key = serde_yaml::Value::String("out".to_string());
            if let Some(serde_yaml::Value::Sequence(seq)) = map.get(&out_key).cloned() {
                let stripped: Vec<serde_yaml::Value> = seq.into_iter().map(|item| {
                    if let serde_yaml::Value::String(s) = item {
                        serde_yaml::Value::String(strip_packed_id_last(&s))
                    } else {
                        item
                    }
                }).collect();
                map.insert(out_key, serde_yaml::Value::Sequence(stripped));
            }

            // Recursively process all values
            let mut new_map = serde_yaml::Mapping::new();
            for (k, v) in map {
                new_map.insert(k, strip_packed_prefixes(v));
            }
            serde_yaml::Value::Mapping(new_map)
        }
        serde_yaml::Value::Sequence(seq) => {
            serde_yaml::Value::Sequence(seq.into_iter().map(strip_packed_prefixes).collect())
        }
        other => other,
    }
}

/// Strip packed-format prefix, keeping the last segment only.
/// Only strips if the ID starts with `#` (packed format indicator).
/// `#main/input` -> `input`
/// `#main/rev/input` -> `input`
/// `#main/rev` -> `rev`
/// `echo_out` -> `echo_out` (no change, not packed)
fn strip_packed_id_last(id: &str) -> String {
    if !id.starts_with('#') {
        return id.to_string();
    }
    let bare = &id[1..]; // strip '#'
    if let Some(last_slash) = bare.rfind('/') {
        bare[last_slash + 1..].to_string()
    } else {
        bare.to_string()
    }
}

/// Strip packed-format prefix, keeping everything after the first segment.
/// Only strips if the ID starts with `#` (packed format indicator).
/// `#main/input` -> `input`
/// `#main/rev/output` -> `rev/output`
/// `#revtool.cwl` -> `revtool.cwl`
/// `step1/echo_out` -> `step1/echo_out` (no change, not packed)
fn strip_packed_id_first(id: &str) -> String {
    if !id.starts_with('#') {
        return id.to_string();
    }
    let bare = &id[1..]; // strip '#'
    if let Some(first_slash) = bare.find('/') {
        bare[first_slash + 1..].to_string()
    } else {
        bare.to_string()
    }
}

/// Parse a CWL document from a string.
///
/// Strips a leading `#!/usr/bin/env cwl-runner` shebang line if present.
/// Resolves namespace prefixes in format fields using `$namespaces`.
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

    // Extract $namespaces before parsing the document
    let namespaces = extract_namespaces(content);

    let mut doc: CwlDocument =
        serde_yaml::from_str(content).context("failed to parse CWL document")?;

    // Resolve namespace prefixes in format fields
    if !namespaces.is_empty() {
        resolve_format_namespaces(&mut doc, &namespaces);
    }

    Ok(doc)
}

/// Extract `$namespaces` from a CWL YAML string.
fn extract_namespaces(content: &str) -> std::collections::HashMap<String, String> {
    let mut ns = std::collections::HashMap::new();
    if let Ok(raw) = serde_yaml::from_str::<serde_yaml::Value>(content) {
        if let Some(ns_map) = raw.get("$namespaces").and_then(|v| v.as_mapping()) {
            for (key, val) in ns_map {
                if let (Some(k), Some(v)) = (key.as_str(), val.as_str()) {
                    ns.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    ns
}

/// Resolve namespace prefixes in format fields of a CWL document.
fn resolve_format_namespaces(doc: &mut CwlDocument, namespaces: &std::collections::HashMap<String, String>) {
    match doc {
        CwlDocument::CommandLineTool(tool) => {
            for output in tool.outputs.values_mut() {
                if let Some(ref mut fmt) = output.format {
                    *fmt = resolve_ns_prefix(fmt, namespaces);
                }
            }
        }
        CwlDocument::Workflow(_wf) => {
            // Workflow outputs don't typically have format
        }
        CwlDocument::ExpressionTool(_) => {}
    }
}

/// Resolve a namespace prefix like `edam:format_2330` using the namespace map.
fn resolve_ns_prefix(value: &str, namespaces: &std::collections::HashMap<String, String>) -> String {
    if let Some(colon_pos) = value.find(':') {
        let prefix = &value[..colon_pos];
        let local = &value[colon_pos + 1..];
        if let Some(base_uri) = namespaces.get(prefix) {
            return format!("{}{}", base_uri, local);
        }
    }
    value.to_string()
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

/// Check whether a CommandLineTool has NetworkAccess requirement.
pub fn has_network_access(tool: &CommandLineTool) -> bool {
    tool.requirements.iter().any(|req| {
        req.get("class").and_then(|v| v.as_str()) == Some("NetworkAccess")
    })
}

/// Extract EnvVarRequirement envDef entries from a CommandLineTool.
///
/// Returns a vec of `(var_name, value_template)` pairs where value_template
/// may contain `$(inputs.X)` parameter references.
pub fn env_var_requirement(tool: &CommandLineTool) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for entry in tool.requirements.iter().chain(tool.hints.iter()) {
        if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
            if class == "EnvVarRequirement" {
                if let Some(env_def) = entry.get("envDef") {
                    // envDef can be a mapping {NAME: value} or a list [{envName: NAME, envValue: value}]
                    if let Some(mapping) = env_def.as_mapping() {
                        for (key, val) in mapping {
                            if let (Some(k), Some(v)) = (key.as_str(), val.as_str()) {
                                result.push((k.to_string(), v.to_string()));
                            }
                        }
                    } else if let Some(items) = env_def.as_sequence() {
                        for item in items {
                            let name = item
                                .get("envName")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let value = item
                                .get("envValue")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !name.is_empty() {
                                result.push((name.to_string(), value.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }
    result
}

/// Extract InitialWorkDirRequirement listing entries from a CommandLineTool.
///
/// Returns a vec of `(entryname, entry_content)` pairs. Each entry has an
/// `entryname` (the filename to create) and `entry` (the content, which may
/// contain `$(inputs.X)` parameter references).
pub fn initial_workdir_listing(tool: &CommandLineTool) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for entry in &tool.requirements {
        if let Some(class) = entry.get("class").and_then(|v| v.as_str()) {
            if class == "InitialWorkDirRequirement" {
                if let Some(listing) = entry.get("listing") {
                    if let Some(items) = listing.as_sequence() {
                        for item in items {
                            let entryname = item
                                .get("entryname")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let entry_content = item
                                .get("entry")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !entryname.is_empty() && !entry_content.is_empty() {
                                result.push((
                                    entryname.to_string(),
                                    entry_content.to_string(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    result
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

    #[test]
    fn parse_initial_workdir_listing() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  - class: InitialWorkDirRequirement
    listing:
      - entryname: config.txt
        entry: "threads=$(inputs.count)"
      - entryname: run.sh
        entry: "echo hello"
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let listing = initial_workdir_listing(&tool);
                assert_eq!(listing.len(), 2);
                assert_eq!(listing[0].0, "config.txt");
                assert_eq!(listing[0].1, "threads=$(inputs.count)");
                assert_eq!(listing[1].0, "run.sh");
                assert_eq!(listing[1].1, "echo hello");
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_initial_workdir_listing_empty() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let listing = initial_workdir_listing(&tool);
                assert!(listing.is_empty());
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_map_form_requirements_docker_image() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  DockerRequirement:
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
    fn parse_map_form_resource_requirement() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  ResourceRequirement:
    coresMin: 8
    ramMin: 4096
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let (cores, ram) = resource_requirement(&tool);
                assert_eq!(cores, 8);
                assert_eq!(ram, 4096);
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_list_form_tool_inputs() {
        let yaml = r#"
class: CommandLineTool
baseCommand: bwa
inputs:
  - id: reference
    type: File
    inputBinding: { position: 2 }
  - id: reads
    type:
      type: array
      items: File
    inputBinding: { position: 3 }
outputs: {}
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                assert!(tool.inputs.contains_key("reference"));
                assert!(tool.inputs.contains_key("reads"));
                assert_eq!(tool.inputs["reference"].cwl_type.base_type(), "File");
                assert!(tool.inputs["reads"].cwl_type.is_array());
                assert_eq!(tool.inputs["reads"].cwl_type.base_type(), "File");
            }
            _ => panic!("expected CommandLineTool"),
        }
    }

    #[test]
    fn parse_initial_workdir_listing_map_form() {
        let yaml = r#"
class: CommandLineTool
baseCommand: echo
inputs: {}
outputs: {}
requirements:
  InitialWorkDirRequirement:
    listing:
      - entryname: script.sh
        entry: "echo $(inputs.msg)"
"#;
        let doc = parse_cwl_str(yaml).unwrap();
        match doc {
            CwlDocument::CommandLineTool(tool) => {
                let listing = initial_workdir_listing(&tool);
                assert_eq!(listing.len(), 1);
                assert_eq!(listing[0].0, "script.sh");
                assert_eq!(listing[0].1, "echo $(inputs.msg)");
            }
            _ => panic!("expected CommandLineTool"),
        }
    }
}
