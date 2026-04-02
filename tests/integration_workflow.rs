use std::path::Path;

use cwl_zen::model::CwlDocument;
use cwl_zen::{dag, execute, input, parse, provenance};

#[test]
fn run_two_step_workflow_with_provenance() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = manifest_dir.join("tests/fixtures");

    // 1. Parse tests/fixtures/two-step.cwl
    let wf_path = fixtures.join("two-step.cwl");
    let doc = parse::parse_cwl(&wf_path).expect("failed to parse two-step.cwl");
    let wf = match doc {
        CwlDocument::Workflow(wf) => wf,
        _ => panic!("expected Workflow"),
    };

    // 2. Build DAG
    let dag_steps = dag::build_dag(&wf).expect("failed to build DAG");
    assert_eq!(dag_steps.len(), 2, "two-step workflow should have 2 DAG steps");

    // 3. Parse tests/fixtures/two-step-input.yml (base_dir = "tests/fixtures")
    let input_path = fixtures.join("two-step-input.yml");
    let inputs = input::parse_inputs(&input_path, &fixtures).expect("failed to parse inputs");

    // 4. Create tempdir as outdir
    let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
    let outdir = tmpdir.path().join("output");

    // 5. Execute workflow
    let run_result = execute::execute_workflow(&wf_path, &wf, &dag_steps, &inputs, &outdir)
        .expect("execute_workflow failed");

    // 6. Assert success
    assert!(run_result.success, "workflow should succeed");
    assert_eq!(run_result.steps.len(), 2, "should have 2 step results");
    for step in &run_result.steps {
        assert_eq!(
            step.exit_code, 0,
            "step '{}' should exit with code 0",
            step.step_name
        );
    }

    // 7. Generate RO-Crate provenance
    let crate_dir = tmpdir.path().join("ro-crate");
    provenance::generate_crate(&run_result, &crate_dir).expect("generate_crate failed");

    // 8. Read ro-crate-metadata.json
    let metadata_path = crate_dir.join("ro-crate-metadata.json");
    let metadata_str =
        std::fs::read_to_string(&metadata_path).expect("failed to read ro-crate-metadata.json");

    // 9. Parse as serde_json::Value
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).expect("failed to parse metadata JSON");

    // 10. Verify

    // @graph is non-empty
    let graph = metadata["@graph"]
        .as_array()
        .expect("@graph should be an array");
    assert!(!graph.is_empty(), "@graph should be non-empty");

    // Root dataset conformsTo includes Provenance Run Crate 0.5
    let root_dataset = graph
        .iter()
        .find(|e| e.get("@id") == Some(&serde_json::json!("./")))
        .expect("root dataset './' not found");
    let conforms_to = root_dataset["conformsTo"]
        .as_array()
        .expect("conformsTo should be an array");
    let provenance_profile = "https://w3id.org/ro/wfrun/provenance/0.5";
    assert!(
        conforms_to
            .iter()
            .any(|v| v.get("@id").and_then(|id| id.as_str()) == Some(provenance_profile)),
        "root dataset must conform to Provenance Run Crate 0.5, got: {:?}",
        conforms_to
    );

    // At least 3 CreateActions (1 workflow + 2 steps)
    let create_actions: Vec<_> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("CreateAction"))
        .collect();
    assert!(
        create_actions.len() >= 3,
        "should have at least 3 CreateActions (1 workflow + 2 steps), got {}",
        create_actions.len()
    );

    // Has OrganizeAction
    let organize_actions: Vec<_> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("OrganizeAction"))
        .collect();
    assert!(
        !organize_actions.is_empty(),
        "should have at least one OrganizeAction"
    );

    // Has 2 ControlActions (one per step)
    let control_actions: Vec<_> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("ControlAction"))
        .collect();
    assert_eq!(
        control_actions.len(),
        2,
        "should have 2 ControlActions (one per step), got {}",
        control_actions.len()
    );

    // Has 2 HowToStep entities
    let howto_steps: Vec<_> = graph
        .iter()
        .filter(|e| e.get("@type").and_then(|t| t.as_str()) == Some("HowToStep"))
        .collect();
    assert_eq!(
        howto_steps.len(),
        2,
        "should have 2 HowToStep entities, got {}",
        howto_steps.len()
    );

    // No timestamps end with 'Z' (must end with +00:00)
    let timestamp_fields = ["startTime", "endTime", "datePublished", "dateModified"];
    for entity in graph {
        for field in &timestamp_fields {
            if let Some(ts) = entity.get(*field).and_then(|v| v.as_str()) {
                assert!(
                    !ts.ends_with('Z'),
                    "timestamp '{}' in entity {:?} ends with 'Z' (must end with +00:00): {}",
                    field,
                    entity.get("@id"),
                    ts
                );
                assert!(
                    ts.ends_with("+00:00"),
                    "timestamp '{}' in entity {:?} does not end with +00:00: {}",
                    field,
                    entity.get("@id"),
                    ts
                );
            }
        }
    }
}
