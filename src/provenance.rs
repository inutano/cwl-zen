use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::execute::RunResult;
use crate::model::{ResolvedValue, Workflow};
use crate::param;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RO_CRATE_CONTEXT: &str = "https://w3id.org/ro/crate/1.1/context";
const WF_RUN_CONTEXT: &str = "https://w3id.org/ro/terms/workflow-run/context";

const PROFILE_PROCESS: &str = "https://w3id.org/ro/wfrun/process/0.5";
const PROFILE_WORKFLOW: &str = "https://w3id.org/ro/wfrun/workflow/0.5";
const PROFILE_PROVENANCE: &str = "https://w3id.org/ro/wfrun/provenance/0.5";
const PROFILE_WF_RO_CRATE: &str = "https://w3id.org/workflowhub/workflow-ro-crate/1.0";

const RO_CRATE_SPEC: &str = "https://w3id.org/ro/crate/1.1";

// ---------------------------------------------------------------------------
// EDAM extension mapping
// ---------------------------------------------------------------------------

/// Return (EDAM URI, human label) for a file extension, if known.
fn edam_for_extension(ext: &str) -> Option<(&'static str, &'static str)> {
    match ext.to_lowercase().as_str() {
        ".bam" => Some(("http://edamontology.org/format_2572", "BAM")),
        ".sam" => Some(("http://edamontology.org/format_2573", "SAM")),
        ".cram" => Some(("http://edamontology.org/format_3462", "CRAM")),
        ".vcf" => Some(("http://edamontology.org/format_3016", "VCF")),
        ".bcf" => Some(("http://edamontology.org/format_3020", "BCF")),
        ".fastq" | ".fq" => Some(("http://edamontology.org/format_1930", "FASTQ")),
        ".fasta" | ".fa" => Some(("http://edamontology.org/format_1929", "FASTA")),
        ".bed" => Some(("http://edamontology.org/format_3003", "BED")),
        ".gff3" | ".gff" => Some(("http://edamontology.org/format_1975", "GFF3")),
        ".gtf" => Some(("http://edamontology.org/format_2306", "GTF")),
        ".bw" | ".bigwig" => Some(("http://edamontology.org/format_3006", "bigWig")),
        ".bb" | ".bigbed" => Some(("http://edamontology.org/format_3004", "bigBed")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// CWL type → Schema.org additionalType
// ---------------------------------------------------------------------------

fn cwl_type_to_schema_org(cwl_base: &str) -> &'static str {
    match cwl_base {
        "File" => "File",
        "Directory" => "Dataset",
        "string" => "Text",
        "int" | "long" => "Integer",
        "float" | "double" => "Float",
        "boolean" => "Boolean",
        _ => "Text",
    }
}

// ---------------------------------------------------------------------------
// Timestamp formatting
// ---------------------------------------------------------------------------

fn format_timestamp(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

// ---------------------------------------------------------------------------
// SHA-256 of a file (streaming, 64 KB buffer)
// ---------------------------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("opening file for sha256: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// File metadata helper
// ---------------------------------------------------------------------------

fn file_entity(id: &str, path: &Path) -> Value {
    let meta = fs::metadata(path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            format_timestamp(&dt)
        });
    let hash = sha256_file(path).ok();

    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    let mut entity = json!({
        "@id": id,
        "@type": "File",
        "contentSize": size.to_string(),
    });

    if let Some(dt) = modified {
        entity["dateModified"] = json!(dt);
    }
    if let Some(h) = hash {
        entity["sha256"] = json!(h);
    }
    if let Some((edam_uri, _label)) = edam_for_extension(&ext) {
        entity["encodingFormat"] = json!(edam_uri);
    }

    entity
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a Provenance Run Crate (ro-crate-metadata.json) from a RunResult.
pub fn generate_crate(run_result: &RunResult, outdir: &Path) -> Result<()> {
    fs::create_dir_all(outdir)
        .with_context(|| format!("creating crate outdir: {}", outdir.display()))?;

    let mut graph: Vec<Value> = Vec::new();

    // 1. Boilerplate entities
    add_boilerplate(&mut graph);

    // Workflow file ID (relative inside crate)
    let wf_basename = run_result
        .workflow_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workflow.cwl".to_string());

    // 2. Workflow-level entities
    let (_wf_input_fp_ids, _wf_output_fp_ids) =
        add_workflow_entities(&mut graph, &run_result.workflow, &wf_basename);

    // 3. Run-level entities
    let wf_action_id = format!("#run-{}", Uuid::new_v4());
    let organize_action_id = format!("#organize-{}", Uuid::new_v4());

    // Collect input data entity IDs
    let mut input_entity_ids: Vec<String> = Vec::new();
    let mut all_file_ids: Vec<String> = Vec::new();

    // Build input data entities
    for (name, val) in &run_result.inputs {
        let fp_id = format!("#param-input-{}", name);
        let data_id = resolved_value_id(name, val, "input");
        let data_entity = resolved_value_entity(&data_id, val, &fp_id);
        graph.push(data_entity);
        input_entity_ids.push(data_id.clone());
        if is_file_entity(val) {
            all_file_ids.push(data_id);
        }
    }

    // Build output data entities
    let mut output_entity_ids: Vec<String> = Vec::new();
    for (name, val) in &run_result.outputs {
        let fp_id = format!("#param-output-{}", name);
        let data_id = resolved_value_id(name, val, "output");
        let mut data_entity = resolved_value_entity(&data_id, val, &fp_id);
        // Enrich output files with metadata
        if let ResolvedValue::File(fv) = val {
            let p = Path::new(&fv.path);
            if p.exists() {
                let meta_entity = file_entity(&data_id, p);
                // Merge metadata fields into data_entity
                if let (Some(obj), Some(meta_obj)) = (data_entity.as_object_mut(), meta_entity.as_object()) {
                    for (k, v) in meta_obj {
                        if k != "@id" && k != "@type" {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        graph.push(data_entity);
        output_entity_ids.push(data_id.clone());
        if is_file_entity(val) {
            all_file_ids.push(data_id);
        }
    }

    // Workflow CreateAction
    let action_status = if run_result.success {
        "http://schema.org/CompletedActionStatus"
    } else {
        "http://schema.org/FailedActionStatus"
    };
    graph.push(json!({
        "@id": &wf_action_id,
        "@type": "CreateAction",
        "name": "Run workflow",
        "instrument": { "@id": &wf_basename },
        "object": input_entity_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
        "result": output_entity_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
        "startTime": format_timestamp(&run_result.start_time),
        "endTime": format_timestamp(&run_result.end_time),
        "actionStatus": action_status,
    }));

    // 4. Per-step entities
    let mut step_action_ids: Vec<String> = Vec::new();
    let mut control_action_ids: Vec<String> = Vec::new();

    for (idx, step) in run_result.steps.iter().enumerate() {
        let step_action_id = format!("#step-action-{}", step.step_name);
        let control_action_id = format!("#control-{}", step.step_name);
        let howto_step_id = format!("#step-{}", step.step_name);
        let tool_id = format!("#tool-{}", step.step_name);

        // Tool SoftwareApplication
        let tool_basename = step
            .tool_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "tool.cwl".to_string());
        graph.push(json!({
            "@id": &tool_id,
            "@type": "SoftwareApplication",
            "name": &tool_basename,
        }));

        // Step input/output entities
        let mut step_input_ids: Vec<String> = Vec::new();
        for (name, val) in &step.inputs {
            let sid = format!("#step-{}-input-{}", step.step_name, name);
            let entity = resolved_value_entity_simple(&sid, val);
            graph.push(entity);
            step_input_ids.push(sid.clone());
            if is_file_entity(val) {
                all_file_ids.push(sid);
            }
        }

        let mut step_output_ids: Vec<String> = Vec::new();
        for (name, val) in &step.outputs {
            let sid = format!("#step-{}-output-{}", step.step_name, name);
            let mut entity = resolved_value_entity_simple(&sid, val);
            // Enrich with file metadata
            if let ResolvedValue::File(fv) = val {
                let p = Path::new(&fv.path);
                if p.exists() {
                    let meta_entity = file_entity(&sid, p);
                    if let (Some(obj), Some(meta_obj)) = (entity.as_object_mut(), meta_entity.as_object()) {
                        for (k, v) in meta_obj {
                            if k != "@id" && k != "@type" {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
            graph.push(entity);
            step_output_ids.push(sid.clone());
            if is_file_entity(val) {
                all_file_ids.push(sid);
            }
        }

        // Log file entities
        let mut log_ids: Vec<String> = Vec::new();
        if let Some(ref stdout_path) = step.stdout_path {
            let log_id = format!("logs/{}.stdout.log", step.step_name);
            if stdout_path.exists() {
                graph.push(file_entity(&log_id, stdout_path));
                log_ids.push(log_id.clone());
                all_file_ids.push(log_id);
            }
        }
        if let Some(ref stderr_path) = step.stderr_path {
            let log_id = format!("logs/{}.stderr.log", step.step_name);
            if stderr_path.exists() {
                graph.push(file_entity(&log_id, stderr_path));
                log_ids.push(log_id.clone());
                all_file_ids.push(log_id);
            }
        }

        // ContainerImage (if docker used)
        let container_image_id = step.container_image.as_ref().map(|img| {
            let cid = format!("#container-{}", step.step_name);
            graph.push(json!({
                "@id": &cid,
                "@type": "ContainerImage",
                "name": img,
            }));
            cid
        });

        // Step CreateAction
        let step_status = if step.exit_code == 0 {
            "http://schema.org/CompletedActionStatus"
        } else {
            "http://schema.org/FailedActionStatus"
        };
        let mut step_action = json!({
            "@id": &step_action_id,
            "@type": "CreateAction",
            "name": format!("Run step {}", step.step_name),
            "instrument": { "@id": &tool_id },
            "object": step_input_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
            "result": step_output_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
            "startTime": format_timestamp(&step.start_time),
            "endTime": format_timestamp(&step.end_time),
            "actionStatus": step_status,
            "exitCode": step.exit_code,
        });
        if !log_ids.is_empty() {
            step_action["subjectOf"] = json!(
                log_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>()
            );
        }
        if let Some(ref cid) = container_image_id {
            step_action["containerImage"] = json!({"@id": cid});
        }
        graph.push(step_action);

        // HowToStep
        graph.push(json!({
            "@id": &howto_step_id,
            "@type": "HowToStep",
            "position": (idx + 1).to_string(),
            "name": &step.step_name,
        }));

        // ControlAction
        graph.push(json!({
            "@id": &control_action_id,
            "@type": "ControlAction",
            "instrument": { "@id": &howto_step_id },
            "object": { "@id": &step_action_id },
        }));

        step_action_ids.push(step_action_id);
        control_action_ids.push(control_action_id);
    }

    // OrganizeAction
    graph.push(json!({
        "@id": &organize_action_id,
        "@type": "OrganizeAction",
        "name": "Orchestrate workflow",
        "instrument": { "@id": "#cwl-zen" },
        "object": { "@id": &wf_action_id },
        "result": control_action_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
    }));

    // 6. Tataki enrichment (optional) — try running tataki
    try_tataki_enrichment(&mut graph, outdir);

    // 7. Finalize root dataset
    // Collect all File-type @ids for hasPart (first pass)
    let has_part_ids: Vec<String> = {
        let mut ids = Vec::new();
        ids.push(wf_basename.clone());
        for entity in &graph {
            if let Some(at_type) = entity.get("@type") {
                let is_file = match at_type {
                    Value::String(s) => s == "File",
                    Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some("File")),
                    _ => false,
                };
                if is_file {
                    if let Some(id) = entity.get("@id").and_then(|v| v.as_str()) {
                        // Avoid duplicates
                        let id_s = id.to_string();
                        if !ids.contains(&id_s) {
                            ids.push(id_s);
                        }
                    }
                }
            }
        }
        ids
    };

    // Collect all mention-worthy action @ids
    let mut mention_ids: Vec<String> = Vec::new();
    mention_ids.push(wf_action_id.clone());
    mention_ids.push(organize_action_id.clone());
    mention_ids.extend(step_action_ids.iter().cloned());
    mention_ids.extend(control_action_ids.iter().cloned());

    // Second pass: update root dataset
    for entity in graph.iter_mut() {
        if entity.get("@id") == Some(&json!("./")) {
            entity["hasPart"] =
                json!(has_part_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>());
            entity["mainEntity"] = json!({"@id": &wf_basename});
            entity["mentions"] =
                json!(mention_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>());
            break;
        }
    }

    // Copy workflow CWL file into crate
    if run_result.workflow_path.exists() {
        let dest = outdir.join(&wf_basename);
        let _ = fs::copy(&run_result.workflow_path, &dest);
    }

    // Copy tool CWL files into crate
    for step in &run_result.steps {
        if step.tool_path.exists() {
            let tool_basename = step
                .tool_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !tool_basename.is_empty() {
                let dest = outdir.join(&tool_basename);
                let _ = fs::copy(&step.tool_path, &dest);
            }
        }
    }

    // Write ro-crate-metadata.json
    let crate_doc = json!({
        "@context": [RO_CRATE_CONTEXT, WF_RUN_CONTEXT],
        "@graph": graph,
    });

    let json_str = serde_json::to_string_pretty(&crate_doc)?;
    let metadata_path = outdir.join("ro-crate-metadata.json");
    fs::write(&metadata_path, json_str)
        .with_context(|| format!("writing {}", metadata_path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: boilerplate entities
// ---------------------------------------------------------------------------

fn add_boilerplate(graph: &mut Vec<Value>) {
    // Metadata descriptor
    graph.push(json!({
        "@id": "ro-crate-metadata.json",
        "@type": "CreativeWork",
        "about": { "@id": "./" },
        "conformsTo": [
            { "@id": RO_CRATE_SPEC },
            { "@id": PROFILE_WF_RO_CRATE }
        ],
    }));

    // Root dataset
    graph.push(json!({
        "@id": "./",
        "@type": "Dataset",
        "conformsTo": [
            { "@id": PROFILE_PROCESS },
            { "@id": PROFILE_WORKFLOW },
            { "@id": PROFILE_PROVENANCE },
            { "@id": PROFILE_WF_RO_CRATE }
        ],
        "datePublished": format_timestamp(&Utc::now()),
        "license": "https://spdx.org/licenses/MIT",
    }));

    // Profile CreativeWork stubs
    for profile in &[PROFILE_PROCESS, PROFILE_WORKFLOW, PROFILE_PROVENANCE, PROFILE_WF_RO_CRATE] {
        graph.push(json!({
            "@id": profile,
            "@type": "CreativeWork",
            "name": profile.rsplit('/').next().unwrap_or(profile),
            "version": profile.rsplit('/').next().unwrap_or("0.5"),
        }));
    }

    // ComputerLanguage for CWL v1.2
    graph.push(json!({
        "@id": "#cwl",
        "@type": "ComputerLanguage",
        "name": "Common Workflow Language",
        "version": "v1.2",
        "identifier": "https://w3id.org/cwl/v1.2/",
        "url": "https://www.commonwl.org/",
    }));

    // SoftwareApplication for cwl-zen
    graph.push(json!({
        "@id": "#cwl-zen",
        "@type": "SoftwareApplication",
        "name": "cwl-zen",
        "version": env!("CARGO_PKG_VERSION"),
        "url": "https://github.com/inutano/cwl-zen",
    }));
}

// ---------------------------------------------------------------------------
// Internal: workflow-level entities
// ---------------------------------------------------------------------------

/// Add ComputationalWorkflow, FormalParameters, HowToSteps.
/// Returns (input_fp_ids, output_fp_ids) for linking.
fn add_workflow_entities(
    graph: &mut Vec<Value>,
    workflow: &Workflow,
    wf_basename: &str,
) -> (Vec<String>, Vec<String>) {
    let mut input_fp_ids: Vec<String> = Vec::new();
    let mut output_fp_ids: Vec<String> = Vec::new();

    // FormalParameters for inputs
    for (name, wf_input) in &workflow.inputs {
        let fp_id = format!("#param-input-{}", name);
        let base = wf_input.cwl_type.base_type();
        let additional_type = cwl_type_to_schema_org(base);
        let value_required = !wf_input.cwl_type.is_optional();
        let multiple_values = wf_input.cwl_type.is_array();

        graph.push(json!({
            "@id": &fp_id,
            "@type": "FormalParameter",
            "name": name,
            "additionalType": additional_type,
            "valueRequired": value_required,
            "multipleValues": multiple_values,
        }));
        input_fp_ids.push(fp_id);
    }

    // FormalParameters for outputs
    for (name, wf_output) in &workflow.outputs {
        let fp_id = format!("#param-output-{}", name);
        let base = wf_output.cwl_type.base_type();
        let additional_type = cwl_type_to_schema_org(base);
        let value_required = !wf_output.cwl_type.is_optional();
        let multiple_values = wf_output.cwl_type.is_array();

        graph.push(json!({
            "@id": &fp_id,
            "@type": "FormalParameter",
            "name": name,
            "additionalType": additional_type,
            "valueRequired": value_required,
            "multipleValues": multiple_values,
        }));
        output_fp_ids.push(fp_id);
    }

    // ComputationalWorkflow entity
    graph.push(json!({
        "@id": wf_basename,
        "@type": ["File", "SoftwareSourceCode", "ComputationalWorkflow"],
        "name": wf_basename,
        "programmingLanguage": { "@id": "#cwl" },
        "input": input_fp_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
        "output": output_fp_ids.iter().map(|id| json!({"@id": id})).collect::<Vec<_>>(),
    }));

    // HowToStep per workflow step
    // Note: Steps are also added per-step in the run section. These are the
    // workflow-definition-level steps referenced by ControlActions.
    // We skip adding them here to avoid duplicates; they're created in the
    // per-step loop in generate_crate().

    (input_fp_ids, output_fp_ids)
}

// ---------------------------------------------------------------------------
// Internal: data entity helpers
// ---------------------------------------------------------------------------

fn resolved_value_id(name: &str, val: &ResolvedValue, prefix: &str) -> String {
    match val {
        ResolvedValue::File(fv) => {
            // Use basename as the entity ID for files
            format!("data/{}-{}", prefix, fv.basename)
        }
        _ => format!("#{}_{}", prefix, name),
    }
}

fn is_file_entity(val: &ResolvedValue) -> bool {
    matches!(val, ResolvedValue::File(_))
}

/// Create a data entity for a resolved value, with exampleOfWork linking to FormalParameter.
fn resolved_value_entity(id: &str, val: &ResolvedValue, fp_id: &str) -> Value {
    match val {
        ResolvedValue::File(fv) => {
            json!({
                "@id": id,
                "@type": "File",
                "name": &fv.basename,
                "exampleOfWork": { "@id": fp_id },
            })
        }
        ResolvedValue::Directory(fv) => {
            json!({
                "@id": id,
                "@type": "Dataset",
                "name": &fv.basename,
                "exampleOfWork": { "@id": fp_id },
            })
        }
        _ => {
            let str_val = param::value_to_string(val);
            json!({
                "@id": id,
                "@type": "PropertyValue",
                "name": id.trim_start_matches('#'),
                "value": str_val,
                "exampleOfWork": { "@id": fp_id },
            })
        }
    }
}

/// Create a simple data entity without exampleOfWork (for step-level I/O).
fn resolved_value_entity_simple(id: &str, val: &ResolvedValue) -> Value {
    match val {
        ResolvedValue::File(fv) => {
            json!({
                "@id": id,
                "@type": "File",
                "name": &fv.basename,
            })
        }
        ResolvedValue::Directory(fv) => {
            json!({
                "@id": id,
                "@type": "Dataset",
                "name": &fv.basename,
            })
        }
        _ => {
            let str_val = param::value_to_string(val);
            json!({
                "@id": id,
                "@type": "PropertyValue",
                "name": id.trim_start_matches('#'),
                "value": str_val,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Internal: tataki enrichment
// ---------------------------------------------------------------------------

fn try_tataki_enrichment(graph: &mut [Value], _outdir: &Path) {
    // Collect file paths from the graph that are actual filesystem paths
    let file_paths: Vec<String> = graph
        .iter()
        .filter_map(|entity| {
            let at_type = entity.get("@type")?;
            let is_file = match at_type {
                Value::String(s) => s == "File",
                Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some("File")),
                _ => false,
            };
            if !is_file {
                return None;
            }
            let id = entity.get("@id")?.as_str()?;
            // Only try tataki on real filesystem paths
            if Path::new(id).exists() {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect();

    if file_paths.is_empty() {
        return;
    }

    // Try running tataki
    let result = std::process::Command::new("tataki")
        .arg("-f")
        .arg("json")
        .arg("-q")
        .args(&file_paths)
        .output();

    let output = match result {
        Ok(o) if o.status.success() => o,
        _ => return, // tataki not available or failed — silently skip
    };

    // Parse tataki output (expected: JSON with file → format mappings)
    let tataki_json: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Apply enrichment: if tataki provides EDAM URIs, update encodingFormat
    if let Some(arr) = tataki_json.as_array() {
        for entry in arr {
            if let (Some(path), Some(format_uri)) = (
                entry.get("path").and_then(|v| v.as_str()),
                entry.get("format").and_then(|v| v.as_str()),
            ) {
                for entity in graph.iter_mut() {
                    if entity.get("@id").and_then(|v| v.as_str()) == Some(path) {
                        entity["encodingFormat"] = json!(format_uri);
                    }
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CwlType, Workflow, WorkflowInput, WorkflowOutput};
    use crate::execute::RunResult;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // -----------------------------------------------------------------------
    // Mock helpers
    // -----------------------------------------------------------------------

    fn mock_workflow() -> Workflow {
        let mut inputs = HashMap::new();
        inputs.insert(
            "sample_id".to_string(),
            WorkflowInput {
                id: None,
                cwl_type: CwlType::Single("string".to_string()),
                secondary_files: Vec::new(),
                doc: None,
                default: None,
            },
        );
        inputs.insert(
            "reads".to_string(),
            WorkflowInput {
                id: None,
                cwl_type: CwlType::Single("File".to_string()),
                secondary_files: Vec::new(),
                doc: None,
                default: None,
            },
        );

        let mut outputs = HashMap::new();
        outputs.insert(
            "bam".to_string(),
            WorkflowOutput {
                id: None,
                cwl_type: CwlType::Single("File".to_string()),
                output_source: Some("align/aligned_bam".to_string()),
                doc: None,
            },
        );

        Workflow {
            cwl_version: Some("v1.2".to_string()),
            label: Some("test workflow".to_string()),
            doc: None,
            inputs,
            outputs,
            steps: HashMap::new(),
            requirements: Vec::new(),
        }
    }

    #[allow(dead_code)]
    fn mock_run_result() -> RunResult {
        RunResult {
            workflow_path: PathBuf::from("/tmp/test-workflow.cwl"),
            workflow: mock_workflow(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            steps: Vec::new(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            success: true,
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: format_timestamp_no_z
    // -----------------------------------------------------------------------

    #[test]
    fn format_timestamp_no_z() {
        let dt = Utc::now();
        let ts = format_timestamp(&dt);
        assert!(
            !ts.ends_with('Z'),
            "timestamp must not end with Z: {}",
            ts
        );
        assert!(
            ts.ends_with("+00:00"),
            "timestamp must end with +00:00: {}",
            ts
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: edam_mapping
    // -----------------------------------------------------------------------

    #[test]
    fn edam_mapping() {
        let (uri, _) = edam_for_extension(".bam").expect(".bam should map");
        assert!(uri.contains("format_2572"), "BAM should be format_2572");

        let (uri, _) = edam_for_extension(".fastq").expect(".fastq should map");
        assert!(uri.contains("format_1930"), "FASTQ should be format_1930");

        assert!(
            edam_for_extension(".txt").is_none(),
            ".txt should not map to EDAM"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: cwl_type_mapping
    // -----------------------------------------------------------------------

    #[test]
    fn cwl_type_mapping() {
        assert_eq!(cwl_type_to_schema_org("File"), "File");
        assert_eq!(cwl_type_to_schema_org("Directory"), "Dataset");
        assert_eq!(cwl_type_to_schema_org("string"), "Text");
        assert_eq!(cwl_type_to_schema_org("int"), "Integer");
        assert_eq!(cwl_type_to_schema_org("long"), "Integer");
        assert_eq!(cwl_type_to_schema_org("float"), "Float");
        assert_eq!(cwl_type_to_schema_org("double"), "Float");
        assert_eq!(cwl_type_to_schema_org("boolean"), "Boolean");
    }

    // -----------------------------------------------------------------------
    // Test 4: boilerplate_has_required_entities
    // -----------------------------------------------------------------------

    #[test]
    fn boilerplate_has_required_entities() {
        let mut graph: Vec<Value> = Vec::new();
        add_boilerplate(&mut graph);

        // Check metadata descriptor
        let meta = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("ro-crate-metadata.json")))
            .expect("metadata descriptor missing");
        assert_eq!(meta["@type"], "CreativeWork");

        // Check root dataset
        let root = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("./")))
            .expect("root dataset missing");
        assert_eq!(root["@type"], "Dataset");
        let conforms = root["conformsTo"].as_array().expect("conformsTo array");
        assert_eq!(conforms.len(), 4, "root must conform to 4 profiles");

        // Check 4 profile stubs
        for profile_uri in &[PROFILE_PROCESS, PROFILE_WORKFLOW, PROFILE_PROVENANCE, PROFILE_WF_RO_CRATE] {
            assert!(
                graph.iter().any(|e| e.get("@id") == Some(&json!(profile_uri))),
                "profile stub missing: {}",
                profile_uri
            );
        }

        // Check CWL language
        let cwl_lang = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("#cwl")))
            .expect("CWL language entity missing");
        assert_eq!(cwl_lang["@type"], "ComputerLanguage");

        // Check cwl-zen app
        let app = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("#cwl-zen")))
            .expect("cwl-zen entity missing");
        assert_eq!(app["@type"], "SoftwareApplication");
        assert_eq!(app["name"], "cwl-zen");
    }

    // -----------------------------------------------------------------------
    // Test 5: formal_parameters_created
    // -----------------------------------------------------------------------

    #[test]
    fn formal_parameters_created() {
        let mut graph: Vec<Value> = Vec::new();
        let wf = mock_workflow();
        let (input_fps, output_fps) = add_workflow_entities(&mut graph, &wf, "test-workflow.cwl");

        // 2 inputs → 2 FormalParameter
        assert_eq!(input_fps.len(), 2, "should have 2 input FormalParameters");

        // 1 output → 1 FormalParameter
        assert_eq!(output_fps.len(), 1, "should have 1 output FormalParameter");

        // Check sample_id param
        let sample_fp = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("#param-input-sample_id")))
            .expect("sample_id FormalParameter missing");
        assert_eq!(sample_fp["@type"], "FormalParameter");
        assert_eq!(sample_fp["additionalType"], "Text"); // string → Text

        // Check reads param
        let reads_fp = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("#param-input-reads")))
            .expect("reads FormalParameter missing");
        assert_eq!(reads_fp["additionalType"], "File"); // File → File
        assert_eq!(reads_fp["valueRequired"], true); // not optional

        // Check bam output param
        let bam_fp = graph
            .iter()
            .find(|e| e.get("@id") == Some(&json!("#param-output-bam")))
            .expect("bam FormalParameter missing");
        assert_eq!(bam_fp["additionalType"], "File");
        assert_eq!(bam_fp["valueRequired"], true);
        assert_eq!(bam_fp["multipleValues"], false);
    }
}
