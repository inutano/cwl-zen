use std::collections::HashMap;
use std::path::Path;

use cwl_zen::container;
use cwl_zen::execute;
use cwl_zen::model::{CwlDocument, ResolvedValue, RuntimeContext};
use cwl_zen::parse;
use cwl_zen::staging::StagingMode;

#[test]
fn run_echo_tool() {
    // 1. Parse tests/fixtures/echo.cwl
    let cwl_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo.cwl");
    let doc = parse::parse_cwl(&cwl_path).expect("failed to parse echo.cwl");
    let tool = match doc {
        CwlDocument::CommandLineTool(t) => t,
        _ => panic!("expected CommandLineTool"),
    };

    // 2. Create inputs: message = "hello-zen"
    let mut inputs = HashMap::new();
    inputs.insert(
        "message".to_string(),
        ResolvedValue::String("hello-zen".to_string()),
    );

    // 3. Create a tempdir as outdir
    let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
    let outdir = tmpdir.path().join("work");
    let log_dir = tmpdir.path().join("logs");

    // 4. Create RuntimeContext with cores=1, ram=1024, outdir, tmpdir
    let runtime = RuntimeContext {
        cores: 1,
        ram: 1024,
        outdir: outdir.to_string_lossy().to_string(),
        tmpdir: tmpdir.path().join("tmp").to_string_lossy().to_string(),
    };

    // 5. Create a default container engine (Docker fallback)
    let engine = container::OciEngine::docker();

    // 6. Call execute::execute_tool
    let (exit_code, _outputs) = execute::execute_tool(
        &tool,
        &inputs,
        &outdir,
        &runtime,
        &log_dir,
        "echo",
        &engine,
        StagingMode::Symlink,
        false,
        None,
    )
    .expect("execute_tool failed");

    // 7. Assert exit_code == 0
    assert_eq!(exit_code, 0, "echo tool should exit with code 0");

    // 8. Read logs/echo.stdout.log and assert it contains "hello-zen"
    let stdout_log = log_dir.join("echo.stdout.log");
    let stdout_content =
        std::fs::read_to_string(&stdout_log).expect("failed to read echo.stdout.log");
    assert!(
        stdout_content.contains("hello-zen"),
        "stdout log should contain 'hello-zen', got: {:?}",
        stdout_content
    );
}
