use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::command;
use crate::container::{self, ContainerEngine, ContainerExecRequest};
use crate::dag::DagStep;
use crate::model::{
    CwlDocument, ExpressionTool, FileValue, ResolvedValue, RuntimeContext, StepInput, Workflow,
};
use crate::param;
use crate::parse;
use crate::stage;
use crate::staging::{self, StagingMode};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Execute a single CommandLineTool step.
///
/// 1. Create workdir and log_dir
/// 2. Stage inputs into workdir
/// 3. Build command via `command::build_command()` using staged inputs
/// 4. Set up stdout/stderr log files
/// 5. Run via ContainerEngine (if docker_image set) or directly
/// 6. Capture exit code
/// 7. Auto-retry with copy staging if symlink failure detected
/// 8. Handle stdout redirect
/// 9. Collect outputs via `stage::collect_outputs()`
/// 10. Return (exit_code, outputs)
pub fn execute_tool(
    tool: &crate::model::CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    runtime: &RuntimeContext,
    log_dir: &Path,
    step_name: &str,
    engine: &dyn ContainerEngine,
    staging_mode: StagingMode,
    no_retry_copy: bool,
    tool_dir: Option<&Path>,
) -> Result<(i32, HashMap<String, ResolvedValue>)> {
    // 1. Create workdir and log_dir
    fs::create_dir_all(workdir)
        .with_context(|| format!("creating workdir: {}", workdir.display()))?;
    fs::create_dir_all(log_dir)
        .with_context(|| format!("creating log_dir: {}", log_dir.display()))?;

    // 1b. Merge tool-level defaults for missing inputs
    let mut inputs = inputs.clone();
    for (name, input_def) in &tool.inputs {
        if !inputs.contains_key(name) {
            if let Some(default) = &input_def.default {
                let mut resolved = yaml_to_resolved(default);
                // If it's a File default with a relative path, resolve against tool's directory
                if let ResolvedValue::File(ref mut fv) = resolved {
                    if let Some(tdir) = tool_dir {
                        if !Path::new(&fv.path).is_absolute() {
                            let abs_path = tdir.join(&fv.path);
                            let abs_str = abs_path.to_string_lossy().to_string();
                            *fv = FileValue::from_path(&abs_str);
                        }
                    }
                }
                inputs.insert(name.clone(), resolved);
            }
        }
    }

    // 2. Stage inputs into workdir
    let staged_inputs = staging::stage_inputs(&inputs, workdir, staging_mode)?;

    // 2b. Materialize InitialWorkDirRequirement listing entries
    for (name, content) in parse::initial_workdir_listing(tool) {
        let resolved = param::resolve_param_refs(&content, &staged_inputs, runtime, None);
        let path = workdir.join(&name);
        fs::write(&path, &resolved).with_context(|| {
            format!(
                "writing InitialWorkDirRequirement entry '{}' to {}",
                name,
                path.display()
            )
        })?;
    }

    // 3. Build command using staged inputs
    let resolved_cmd = command::build_command(tool, &staged_inputs, runtime);

    // 4. Set up log files
    let stdout_log = log_dir.join(format!("{}.stdout.log", step_name));
    let stderr_log = log_dir.join(format!("{}.stderr.log", step_name));

    // 5 & 6. Execute the command
    let exit_code = if let Some(ref image) = resolved_cmd.docker_image {
        // Pull image
        let cache_dir = container::resolve_container_cache(None);
        engine.pull(image, &cache_dir)?;

        // Build mounts
        let mounts = container::build_mounts(&staged_inputs, workdir);

        // Execute via engine
        let stdout_file = fs::File::create(&stdout_log)
            .with_context(|| format!("creating stdout log: {}", stdout_log.display()))?;
        let stderr_file = fs::File::create(&stderr_log)
            .with_context(|| format!("creating stderr log: {}", stderr_log.display()))?;

        let req = ContainerExecRequest {
            image: image.clone(),
            args: resolved_cmd.args.clone(),
            use_shell: resolved_cmd.use_shell,
            workdir: workdir.to_path_buf(),
            mounts,
            network: resolved_cmd.network_access,
            cores: resolved_cmd.cores,
            ram: resolved_cmd.ram,
            stdout: stdout_file,
            stderr: stderr_file,
        };
        let status = engine.exec(req)?;
        status.code().unwrap_or(1)
    } else {
        // Direct execution (no container)
        let mut cmd = if resolved_cmd.use_shell {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&resolved_cmd.command_line());
            c
        } else {
            if resolved_cmd.args.is_empty() {
                bail!("empty command line for step '{}'", step_name);
            }
            let mut c = Command::new(&resolved_cmd.args[0]);
            for arg in &resolved_cmd.args[1..] {
                c.arg(arg);
            }
            c
        };

        let stdout_file = fs::File::create(&stdout_log)
            .with_context(|| format!("creating stdout log: {}", stdout_log.display()))?;
        let stderr_file = fs::File::create(&stderr_log)
            .with_context(|| format!("creating stderr log: {}", stderr_log.display()))?;

        cmd.stdout(stdout_file).stderr(stderr_file);
        cmd.current_dir(workdir);

        // Redirect stdin from file if specified (skip if resolved to "null" or empty)
        if let Some(ref stdin_path) = resolved_cmd.stdin_file {
            if stdin_path != "null" && !stdin_path.is_empty() {
                let stdin_f = fs::File::open(stdin_path)
                    .with_context(|| format!("opening stdin file: {}", stdin_path))?;
                cmd.stdin(stdin_f);
            }
        }

        let status = cmd
            .status()
            .with_context(|| format!("running command for step '{}'", step_name))?;
        status.code().unwrap_or(1)
    };

    // 7. Auto-retry with copy staging if symlink failure detected
    if exit_code != 0 && !no_retry_copy && staging_mode == StagingMode::Symlink {
        let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
        if staging::is_symlink_error(&stderr_content) {
            eprintln!(
                "Step '{}' failed with possible symlink error. Retrying with copy-staged inputs...",
                step_name
            );

            // Create a new workdir for the retry
            let retry_workdir = workdir
                .parent()
                .unwrap_or(workdir)
                .join(format!("{}_copy_retry", step_name));
            fs::create_dir_all(&retry_workdir)?;

            // Re-run with Copy mode and no_retry_copy=true to avoid infinite recursion
            let (retry_exit_code, retry_outputs) = execute_tool(
                tool,
                &inputs,
                &retry_workdir,
                runtime,
                log_dir,
                &format!("{}_retry", step_name),
                engine,
                StagingMode::Copy,
                true,
                tool_dir,
            )?;

            if retry_exit_code == 0 {
                eprintln!("Step '{}' succeeded after copy-staging.", step_name);
            }

            return Ok((retry_exit_code, retry_outputs));
        }
    }

    // 8. If stdout redirect: copy stdout log to workdir/{stdout_file}
    if let Some(ref stdout_filename) = resolved_cmd.stdout_file {
        let dest = workdir.join(stdout_filename);
        fs::copy(&stdout_log, &dest).with_context(|| {
            format!(
                "copying stdout log to {}: {}",
                dest.display(),
                stdout_log.display()
            )
        })?;
    }

    // 8b. If stderr redirect: copy stderr log to workdir/{stderr_file}
    // Also handle auto-generated stderr filename when an output has type: stderr
    // but no explicit stderr field on the tool.
    let stderr_filename = if resolved_cmd.stderr_file.is_some() {
        resolved_cmd.stderr_file.clone()
    } else {
        // Check if any output has type: stderr
        let has_stderr_output = tool.outputs.values().any(|o| o.cwl_type.base_type() == "stderr");
        if has_stderr_output {
            Some(format!("{}.stderr", uuid::Uuid::new_v4()))
        } else {
            None
        }
    };
    if let Some(ref stderr_fname) = stderr_filename {
        let dest = workdir.join(stderr_fname);
        fs::copy(&stderr_log, &dest).with_context(|| {
            format!(
                "copying stderr log to {}: {}",
                dest.display(),
                stderr_log.display()
            )
        })?;
    }

    // 8c. Similarly, handle auto-generated stdout filename for type: stdout
    // when no explicit stdout field on the tool.
    if resolved_cmd.stdout_file.is_none() {
        let has_stdout_output = tool.outputs.values().any(|o| o.cwl_type.base_type() == "stdout");
        if has_stdout_output {
            let auto_name = format!("{}.stdout", uuid::Uuid::new_v4());
            let dest = workdir.join(&auto_name);
            fs::copy(&stdout_log, &dest).with_context(|| {
                format!(
                    "copying stdout log to {}: {}",
                    dest.display(),
                    stdout_log.display()
                )
            })?;
        }
    }

    // 9. Check for cwl.output.json (CWL spec: tool can write outputs as JSON)
    let cwl_output_json_path = workdir.join("cwl.output.json");
    if cwl_output_json_path.exists() {
        let json_str = fs::read_to_string(&cwl_output_json_path)
            .with_context(|| format!("reading cwl.output.json from {}", cwl_output_json_path.display()))?;
        let json_val: serde_json::Value = serde_json::from_str(&json_str)
            .with_context(|| "parsing cwl.output.json")?;
        if let serde_json::Value::Object(map) = json_val {
            let mut outputs = HashMap::new();
            for (key, val) in map {
                outputs.insert(key, json_to_resolved_value(&val, workdir));
            }
            return Ok((exit_code, outputs));
        }
    }

    // 10. Fall through to normal glob-based output collection
    let outputs = stage::collect_outputs(tool, &staged_inputs, runtime, workdir)?;

    // 11. Return
    Ok((exit_code, outputs))
}

/// Execute a full workflow: resolve DAG, run steps in order, collect outputs.
pub fn execute_workflow(
    workflow_path: &Path,
    workflow: &Workflow,
    dag: &[DagStep],
    inputs: &HashMap<String, ResolvedValue>,
    outdir: &Path,
    engine: &dyn ContainerEngine,
    staging_mode: StagingMode,
    no_retry_copy: bool,
) -> Result<RunResult> {
    let start_time = Utc::now();

    // Create output and log directories
    fs::create_dir_all(outdir)
        .with_context(|| format!("creating outdir: {}", outdir.display()))?;
    let log_dir = outdir.join("logs");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log dir: {}", log_dir.display()))?;

    // Merge workflow defaults: if an input is not provided, use the workflow
    // input's default value (if declared).
    let mut inputs = inputs.clone();
    for (name, wf_input) in &workflow.inputs {
        if !inputs.contains_key(name) {
            if let Some(default) = &wf_input.default {
                inputs.insert(name.clone(), yaml_to_resolved(default));
            }
        }
    }

    // Resolve the workflow's parent directory for relative tool paths
    let wf_dir = workflow_path
        .parent()
        .unwrap_or_else(|| Path::new("."));

    // Store step outputs for downstream resolution
    let mut step_outputs: HashMap<String, HashMap<String, ResolvedValue>> = HashMap::new();
    let mut step_results: Vec<StepResult> = Vec::new();
    let mut success = true;

    for dag_step in dag {
        let step_start = Utc::now();
        let step_name = &dag_step.name;

        // a. Parse the tool CWL file (relative to workflow directory)
        let tool_path = wf_dir.join(&dag_step.tool_path);
        let doc = parse::parse_cwl(&tool_path)
            .with_context(|| format!("parsing tool for step '{}': {}", step_name, tool_path.display()))?;
        // b. Resolve step inputs (needed by both CommandLineTool and ExpressionTool)
        let wf_step = workflow
            .steps
            .get(step_name)
            .with_context(|| format!("step '{}' not found in workflow", step_name))?;
        let step_inputs = resolve_step_inputs(
            &wf_step.inputs,
            &inputs,
            &step_outputs,
            &RuntimeContext {
                cores: 1,
                ram: 1024,
                outdir: outdir.to_string_lossy().to_string(),
                tmpdir: outdir.join("tmp").to_string_lossy().to_string(),
            },
        )?;

        // Handle ExpressionTool steps directly (no shell execution needed)
        if let CwlDocument::ExpressionTool(ref expr_tool) = doc {
            let expr_runtime = RuntimeContext {
                cores: 1,
                ram: 1024,
                outdir: outdir.to_string_lossy().to_string(),
                tmpdir: outdir.join("tmp").to_string_lossy().to_string(),
            };
            let expr_outputs = execute_expression_tool(expr_tool, &step_inputs, &expr_runtime)?;

            let step_end = Utc::now();
            step_results.push(StepResult {
                step_name: step_name.clone(),
                tool_path: tool_path.clone(),
                container_image: None,
                start_time: step_start,
                end_time: step_end,
                exit_code: 0,
                inputs: step_inputs,
                outputs: expr_outputs.clone(),
                stdout_path: None,
                stderr_path: None,
            });
            step_outputs.insert(step_name.clone(), expr_outputs);
            continue;
        }

        let tool = match doc {
            CwlDocument::CommandLineTool(t) => t,
            _ => bail!("step '{}' must reference a CommandLineTool, not a Workflow", step_name),
        };

        // c. Create per-step workdir
        let step_workdir = outdir.join(".steps").join(step_name);
        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: step_workdir.to_string_lossy().to_string(),
            tmpdir: step_workdir.join("tmp").to_string_lossy().to_string(),
        };

        // d. Execute the tool
        let (exit_code, outputs) = execute_tool(
            &tool,
            &step_inputs,
            &step_workdir,
            &runtime,
            &log_dir,
            step_name,
            engine,
            staging_mode,
            no_retry_copy,
            tool_path.canonicalize().ok().as_deref()
                .and_then(|p| p.parent())
                .or_else(|| tool_path.parent())
                .or(Some(Path::new("."))),
        )?;

        let step_end = Utc::now();
        let container_image = parse::docker_image(&tool);

        // e. Record StepResult
        step_results.push(StepResult {
            step_name: step_name.clone(),
            tool_path: tool_path.clone(),
            container_image,
            start_time: step_start,
            end_time: step_end,
            exit_code,
            inputs: step_inputs,
            outputs: outputs.clone(),
            stdout_path: Some(log_dir.join(format!("{}.stdout.log", step_name))),
            stderr_path: Some(log_dir.join(format!("{}.stderr.log", step_name))),
        });

        // f. If non-zero exit, mark as failed and break
        if exit_code != 0 {
            success = false;
            break;
        }

        // g. Store step outputs for downstream steps
        step_outputs.insert(step_name.clone(), outputs);
    }

    // 4. Resolve workflow-level outputs from step outputs
    let mut wf_outputs = HashMap::new();
    for (out_name, out_def) in &workflow.outputs {
        if let Some(ref source) = out_def.output_source {
            let resolved = resolve_source(source, &inputs, &step_outputs);
            wf_outputs.insert(out_name.clone(), resolved);
        } else {
            wf_outputs.insert(out_name.clone(), ResolvedValue::Null);
        }
    }

    // 5. Copy final output files to outdir root and update paths
    let mut output_updates: Vec<(String, ResolvedValue)> = Vec::new();
    for (name, val) in &wf_outputs {
        if let ResolvedValue::File(fv) = val {
            let src = Path::new(&fv.path);
            if src.exists() {
                let dest = outdir.join(&fv.basename);
                // Only copy if source and dest differ
                if src != dest {
                    let _ = fs::copy(src, &dest);
                }
                if dest.exists() {
                    let updated = FileValue::from_path(&dest.to_string_lossy());
                    output_updates.push((name.clone(), ResolvedValue::File(updated)));
                }
            }
        }
    }
    for (name, val) in output_updates {
        wf_outputs.insert(name, val);
    }

    let end_time = Utc::now();

    Ok(RunResult {
        workflow_path: workflow_path.to_path_buf(),
        workflow: workflow.clone(),
        inputs,
        outputs: wf_outputs,
        steps: step_results,
        start_time,
        end_time,
        success,
    })
}

/// Execute an ExpressionTool.
///
/// ExpressionTools require JavaScript evaluation which CWL Zen does not support.
/// However, for simple passthrough patterns we try our best:
///   - `$(inputs.x)` -- direct passthrough of input value
///   - `${return {"output": inputs.x};}` -- simple return of input values
///
/// For anything more complex, returns an error explaining the limitation.
pub fn execute_expression_tool(
    expr_tool: &ExpressionTool,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Result<HashMap<String, ResolvedValue>> {
    let expression = expr_tool
        .expression
        .as_deref()
        .unwrap_or("")
        .trim();

    if expression.is_empty() {
        bail!("ExpressionTool has no expression defined");
    }

    // Try to evaluate simple JS-like expressions.
    // Pattern 1: ${return {"key": inputs.x, ...};}
    // Pattern 2: ${return {"key": parseInt(inputs.x.contents)};}
    if let Some(result) = try_eval_simple_js_return(expression, inputs, runtime) {
        return Ok(result);
    }

    bail!(
        "ExpressionTool requires JavaScript evaluation which is not supported in CWL Zen.\n\
         Expression: {}",
        expression
    )
}

/// Try to evaluate a simple JS `${return {key: expr, ...};}` pattern.
///
/// Supports:
///   - `inputs.x` -> passthrough
///   - `inputs.x.contents` -> string property
///   - `inputs.x.path` -> string property
///   - `inputs.x.size` -> int property
///   - `parseInt(inputs.x.contents)` -> parse int from string
///   - `parseInt(inputs.x)` -> parse int from string/int value
///   - string/number literals
fn try_eval_simple_js_return(
    expression: &str,
    inputs: &HashMap<String, ResolvedValue>,
    runtime: &RuntimeContext,
) -> Option<HashMap<String, ResolvedValue>> {
    // Strip ${ ... }
    let inner = expression.strip_prefix("${")?.strip_suffix('}')?;
    let inner = inner.trim();

    // Must start with "return"
    let body = inner.strip_prefix("return")?.trim();

    // Must be a JSON-like object: { ... };
    let body = body.strip_suffix(';').unwrap_or(body).trim();
    let body = body.strip_prefix('{')?.strip_suffix('}')?.trim();

    // Parse comma-separated key-value pairs: "key": expr, "key2": expr2
    let mut result = HashMap::new();
    let pairs = split_top_level(body, ',');

    for pair in pairs {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }

        // Split on first ':'
        let colon_pos = pair.find(':')?;
        let key = pair[..colon_pos].trim();
        let val_expr = pair[colon_pos + 1..].trim();

        // Strip quotes from key
        let key = key
            .strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
            .unwrap_or(key);

        let resolved = eval_simple_expr(val_expr, inputs, runtime)?;
        result.insert(key.to_string(), resolved);
    }

    Some(result)
}

/// Split a string on a delimiter, respecting parentheses and brackets.
fn split_top_level(s: &str, delim: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        if ch == '(' || ch == '[' || ch == '{' {
            depth += 1;
            current.push(ch);
        } else if ch == ')' || ch == ']' || ch == '}' {
            depth -= 1;
            current.push(ch);
        } else if ch == delim && depth == 0 {
            result.push(current.clone());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Evaluate a simple expression within a `${return ...}` block.
fn eval_simple_expr(
    expr: &str,
    inputs: &HashMap<String, ResolvedValue>,
    _runtime: &RuntimeContext,
) -> Option<ResolvedValue> {
    let expr = expr.trim();

    // Null literal
    if expr == "null" {
        return Some(ResolvedValue::Null);
    }

    // Boolean literals
    if expr == "true" {
        return Some(ResolvedValue::Bool(true));
    }
    if expr == "false" {
        return Some(ResolvedValue::Bool(false));
    }

    // Numeric literal
    if let Ok(n) = expr.parse::<i64>() {
        return Some(ResolvedValue::Int(n));
    }
    if let Ok(f) = expr.parse::<f64>() {
        return Some(ResolvedValue::Float(f));
    }

    // String literal
    if (expr.starts_with('"') && expr.ends_with('"'))
        || (expr.starts_with('\'') && expr.ends_with('\''))
    {
        let s = &expr[1..expr.len() - 1];
        return Some(ResolvedValue::String(s.to_string()));
    }

    // parseInt(expr)
    if let Some(inner) = expr
        .strip_prefix("parseInt(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_simple_expr(inner.trim(), inputs, _runtime)?;
        let n = parse_int_from_value(&inner_val)?;
        return Some(ResolvedValue::Int(n));
    }

    // parseFloat(expr)
    if let Some(inner) = expr
        .strip_prefix("parseFloat(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner_val = eval_simple_expr(inner.trim(), inputs, _runtime)?;
        let f = parse_float_from_value(&inner_val)?;
        return Some(ResolvedValue::Float(f));
    }

    // self[0].contents -- for outputEval context (not applicable here but keep pattern)

    // inputs.X or inputs.X.property
    if let Some(rest) = expr.strip_prefix("inputs.") {
        return resolve_input_value(rest, inputs);
    }

    // Array literal [expr, expr, ...]
    if expr.starts_with('[') && expr.ends_with(']') {
        let inner = &expr[1..expr.len() - 1];
        let items = split_top_level(inner, ',');
        let mut arr = Vec::new();
        for item in items {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            arr.push(eval_simple_expr(item, inputs, _runtime)?);
        }
        return Some(ResolvedValue::Array(arr));
    }

    None
}

/// Resolve an `inputs.X` or `inputs.X.property` reference.
fn resolve_input_value(rest: &str, inputs: &HashMap<String, ResolvedValue>) -> Option<ResolvedValue> {
    if let Some(dot_pos) = rest.find('.') {
        let name = &rest[..dot_pos];
        let property = &rest[dot_pos + 1..];
        let val = inputs.get(name)?;
        match val {
            ResolvedValue::File(fv) | ResolvedValue::Directory(fv) => match property {
                "path" => Some(ResolvedValue::String(fv.path.clone())),
                "basename" => Some(ResolvedValue::String(fv.basename.clone())),
                "nameroot" => Some(ResolvedValue::String(fv.nameroot.clone())),
                "nameext" => Some(ResolvedValue::String(fv.nameext.clone())),
                "size" => Some(ResolvedValue::Int(fv.size as i64)),
                "contents" => Some(ResolvedValue::String(
                    fv.contents.clone().unwrap_or_default(),
                )),
                _ => None,
            },
            _ => None,
        }
    } else {
        inputs.get(rest).cloned()
    }
}

/// Parse an integer from a ResolvedValue (used for parseInt emulation).
fn parse_int_from_value(val: &ResolvedValue) -> Option<i64> {
    match val {
        ResolvedValue::Int(n) => Some(*n),
        ResolvedValue::Float(f) => Some(*f as i64),
        ResolvedValue::String(s) => {
            // parseInt in JS stops at the first non-numeric char
            let s = s.trim();
            let numeric: String = if s.starts_with('-') {
                std::iter::once('-')
                    .chain(s[1..].chars().take_while(|c| c.is_ascii_digit()))
                    .collect()
            } else {
                s.chars().take_while(|c| c.is_ascii_digit()).collect()
            };
            numeric.parse::<i64>().ok()
        }
        _ => None,
    }
}

/// Parse a float from a ResolvedValue (used for parseFloat emulation).
fn parse_float_from_value(val: &ResolvedValue) -> Option<f64> {
    match val {
        ResolvedValue::Int(n) => Some(*n as f64),
        ResolvedValue::Float(f) => Some(*f),
        ResolvedValue::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a serde_json::Value into a ResolvedValue.
///
/// Handles CWL File/Directory objects (with `class` field) as well as
/// primitive types and arrays. Relative paths are resolved against `workdir`.
fn json_to_resolved_value(val: &serde_json::Value, workdir: &Path) -> ResolvedValue {
    match val {
        serde_json::Value::Null => ResolvedValue::Null,
        serde_json::Value::Bool(b) => ResolvedValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ResolvedValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                ResolvedValue::Float(f)
            } else {
                ResolvedValue::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => ResolvedValue::String(s.clone()),
        serde_json::Value::Array(arr) => {
            ResolvedValue::Array(arr.iter().map(|v| json_to_resolved_value(v, workdir)).collect())
        }
        serde_json::Value::Object(map) => {
            let class = map.get("class").and_then(|v| v.as_str());
            if class == Some("File") {
                let path_str = map
                    .get("path")
                    .or(map.get("location"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let path_str = path_str.strip_prefix("file://").unwrap_or(path_str);
                let path = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    workdir.join(path_str)
                };
                ResolvedValue::File(FileValue::from_path(&path.to_string_lossy()))
            } else if class == Some("Directory") {
                let path_str = map
                    .get("path")
                    .or(map.get("location"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let path_str = path_str.strip_prefix("file://").unwrap_or(path_str);
                let path = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    workdir.join(path_str)
                };
                ResolvedValue::Directory(FileValue::from_path(&path.to_string_lossy()))
            } else {
                // Generic object: serialize back to JSON string
                ResolvedValue::String(serde_json::to_string(val).unwrap_or_default())
            }
        }
    }
}

/// Resolve a source reference to a value.
///
/// If the source contains `/`, look up step_outputs[step_name][output_name].
/// Otherwise, look up wf_inputs[name].
fn resolve_source(
    source: &str,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
) -> ResolvedValue {
    if let Some(slash_pos) = source.find('/') {
        let step_name = &source[..slash_pos];
        let output_name = &source[slash_pos + 1..];
        step_outputs
            .get(step_name)
            .and_then(|outputs| outputs.get(output_name))
            .cloned()
            .unwrap_or(ResolvedValue::Null)
    } else {
        wf_inputs
            .get(source)
            .cloned()
            .unwrap_or(ResolvedValue::Null)
    }
}

/// Resolve step inputs from workflow inputs and upstream step outputs.
fn resolve_step_inputs(
    step_in: &HashMap<String, StepInput>,
    wf_inputs: &HashMap<String, ResolvedValue>,
    step_outputs: &HashMap<String, HashMap<String, ResolvedValue>>,
    runtime: &RuntimeContext,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut resolved = HashMap::new();

    for (input_name, step_input) in step_in {
        let value = match step_input {
            StepInput::Source(source) => resolve_source(source, wf_inputs, step_outputs),
            StepInput::Structured(entry) => {
                // Get base value from source or default
                let base_value = if let Some(ref source) = entry.source {
                    let v = resolve_source(source, wf_inputs, step_outputs);
                    if matches!(v, ResolvedValue::Null) {
                        // Fall back to default if source resolves to null
                        entry
                            .default
                            .as_ref()
                            .map(yaml_to_resolved)
                            .unwrap_or(ResolvedValue::Null)
                    } else {
                        v
                    }
                } else {
                    entry
                        .default
                        .as_ref()
                        .map(yaml_to_resolved)
                        .unwrap_or(ResolvedValue::Null)
                };

                // If value_from is set, resolve it using wf_inputs as the
                // inputs context (so $(inputs.x) references work).
                if let Some(ref vf) = entry.value_from {
                    let resolved_str =
                        param::resolve_param_refs(vf, wf_inputs, runtime, Some(&base_value));
                    ResolvedValue::String(resolved_str)
                } else {
                    base_value
                }
            }
        };

        resolved.insert(input_name.clone(), value);
    }

    Ok(resolved)
}

/// Convert serde_yaml::Value to ResolvedValue for handling defaults.
fn yaml_to_resolved(val: &serde_yaml::Value) -> ResolvedValue {
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
            let items = seq.iter().map(yaml_to_resolved).collect();
            ResolvedValue::Array(items)
        }
        serde_yaml::Value::Mapping(map) => {
            // Check for class: File or class: Directory
            let class = map
                .get(serde_yaml::Value::String("class".to_string()))
                .and_then(|v| v.as_str());
            match class {
                Some("File") => {
                    let path = map
                        .get(serde_yaml::Value::String("path".to_string()))
                        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    ResolvedValue::File(FileValue::from_path(path))
                }
                Some("Directory") => {
                    let path = map
                        .get(serde_yaml::Value::String("path".to_string()))
                        .or_else(|| map.get(serde_yaml::Value::String("location".to_string())))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    ResolvedValue::Directory(FileValue::from_path(path))
                }
                _ => ResolvedValue::Null,
            }
        }
        _ => ResolvedValue::Null,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ResolvedValue, StepInput, StepInputEntry};

    /// Helper: build workflow inputs for testing resolve_source.
    fn test_wf_inputs() -> HashMap<String, ResolvedValue> {
        let mut inputs = HashMap::new();
        inputs.insert(
            "sample_id".to_string(),
            ResolvedValue::String("SRX123".to_string()),
        );
        inputs.insert(
            "message".to_string(),
            ResolvedValue::String("hello".to_string()),
        );
        inputs
    }

    /// Helper: build step outputs for testing resolve_source.
    fn test_step_outputs() -> HashMap<String, HashMap<String, ResolvedValue>> {
        let mut step_outputs = HashMap::new();
        let mut align_outputs = HashMap::new();
        align_outputs.insert(
            "aligned_sam".to_string(),
            ResolvedValue::File(FileValue {
                path: "/work/aligned.sam".to_string(),
                basename: "aligned.sam".to_string(),
                nameroot: "aligned".to_string(),
                nameext: ".sam".to_string(),
                size: 4096,
                checksum: None,
                secondary_files: Vec::new(),
                contents: None,
            }),
        );
        step_outputs.insert("align".to_string(), align_outputs);
        step_outputs
    }

    // -- Test 1: resolve_source for workflow input ----------------------------

    #[test]
    fn resolve_source_workflow_input() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("sample_id", &wf_inputs, &step_outputs);
        match result {
            ResolvedValue::String(s) => assert_eq!(s, "SRX123"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    // -- Test 2: resolve_source for step output ------------------------------

    #[test]
    fn resolve_source_step_output() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("align/aligned_sam", &wf_inputs, &step_outputs);
        match result {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.path, "/work/aligned.sam");
                assert_eq!(fv.basename, "aligned.sam");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    // -- Test 3: resolve_step_inputs with default ----------------------------

    #[test]
    fn resolve_step_inputs_with_default() {
        let wf_inputs = HashMap::new(); // no wf inputs provided
        let step_outputs = HashMap::new(); // no upstream outputs

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "threads".to_string(),
            StepInput::Structured(StepInputEntry {
                id: None,
                source: None,
                value_from: None,
                default: Some(serde_yaml::Value::Number(serde_yaml::Number::from(4))),
            }),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();
        match resolved.get("threads") {
            Some(ResolvedValue::Int(n)) => assert_eq!(*n, 4),
            other => panic!("expected Int(4), got {:?}", other),
        }
    }

    // -- Test 4: resolve_step_inputs with value_from -------------------------

    #[test]
    fn resolve_step_inputs_with_value_from() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = HashMap::new();

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "greeting".to_string(),
            StepInput::Structured(StepInputEntry {
                id: None,
                source: Some("message".to_string()),
                value_from: Some("prefix_$(self)".to_string()),
                default: None,
            }),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();
        match resolved.get("greeting") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "prefix_hello"),
            other => panic!("expected String(\"prefix_hello\"), got {:?}", other),
        }
    }

    // -- Test 5: resolve_source for missing source returns Null --------------

    #[test]
    fn resolve_source_missing_returns_null() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let result = resolve_source("nonexistent", &wf_inputs, &step_outputs);
        assert!(matches!(result, ResolvedValue::Null));

        let result = resolve_source("missing_step/output", &wf_inputs, &step_outputs);
        assert!(matches!(result, ResolvedValue::Null));
    }

    // -- Test 6: yaml_to_resolved converts various types --------------------

    #[test]
    fn yaml_to_resolved_types() {
        // String
        let v = serde_yaml::Value::String("hello".to_string());
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::String(s) if s == "hello"));

        // Int
        let v = serde_yaml::Value::Number(serde_yaml::Number::from(42));
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Int(42)));

        // Float
        let v = serde_yaml::Value::Number(serde_yaml::Number::from(3.14));
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Float(f) if (f - 3.14).abs() < 1e-10));

        // Bool
        let v = serde_yaml::Value::Bool(true);
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Bool(true)));

        // Null
        let v = serde_yaml::Value::Null;
        assert!(matches!(yaml_to_resolved(&v), ResolvedValue::Null));

        // Array
        let v = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("a".to_string()),
            serde_yaml::Value::String("b".to_string()),
        ]);
        match yaml_to_resolved(&v) {
            ResolvedValue::Array(arr) => assert_eq!(arr.len(), 2),
            other => panic!("expected Array, got {:?}", other),
        }
    }

    // -- Test 7: resolve_step_inputs with Source ----------------------------

    #[test]
    fn resolve_step_inputs_simple_source() {
        let wf_inputs = test_wf_inputs();
        let step_outputs = test_step_outputs();

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let mut step_in = HashMap::new();
        step_in.insert(
            "msg".to_string(),
            StepInput::Source("message".to_string()),
        );
        step_in.insert(
            "aligned".to_string(),
            StepInput::Source("align/aligned_sam".to_string()),
        );

        let resolved = resolve_step_inputs(&step_in, &wf_inputs, &step_outputs, &runtime).unwrap();

        match resolved.get("msg") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "hello"),
            other => panic!("expected String(\"hello\"), got {:?}", other),
        }

        match resolved.get("aligned") {
            Some(ResolvedValue::File(fv)) => assert_eq!(fv.path, "/work/aligned.sam"),
            other => panic!("expected File, got {:?}", other),
        }
    }

    // -- Test 8: json_to_resolved_value helper ---------------------------------

    #[test]
    fn json_to_resolved_value_primitives() {
        let workdir = std::path::Path::new("/tmp/test_workdir");

        // Null
        let v = json_to_resolved_value(&serde_json::json!(null), workdir);
        assert!(matches!(v, ResolvedValue::Null));

        // Bool
        let v = json_to_resolved_value(&serde_json::json!(true), workdir);
        assert!(matches!(v, ResolvedValue::Bool(true)));

        // Int
        let v = json_to_resolved_value(&serde_json::json!(42), workdir);
        assert!(matches!(v, ResolvedValue::Int(42)));

        // Float
        let v = json_to_resolved_value(&serde_json::json!(3.14), workdir);
        match v {
            ResolvedValue::Float(f) => assert!((f - 3.14).abs() < 1e-10),
            other => panic!("expected Float, got {:?}", other),
        }

        // String
        let v = json_to_resolved_value(&serde_json::json!("hello"), workdir);
        assert!(matches!(v, ResolvedValue::String(ref s) if s == "hello"));
    }

    #[test]
    fn json_to_resolved_value_array() {
        let workdir = std::path::Path::new("/tmp/test_workdir");

        let v = json_to_resolved_value(&serde_json::json!(["echo", "hello", "world"]), workdir);
        match v {
            ResolvedValue::Array(arr) => {
                assert_eq!(arr.len(), 3);
                assert!(matches!(&arr[0], ResolvedValue::String(s) if s == "echo"));
                assert!(matches!(&arr[1], ResolvedValue::String(s) if s == "hello"));
                assert!(matches!(&arr[2], ResolvedValue::String(s) if s == "world"));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn json_to_resolved_value_file_object() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create a file so FileValue::from_path can stat it
        let test_file = workdir.join("output.txt");
        std::fs::write(&test_file, "content").unwrap();

        let v = json_to_resolved_value(
            &serde_json::json!({
                "class": "File",
                "path": test_file.to_str().unwrap()
            }),
            workdir,
        );
        match v {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "output.txt");
                assert!(fv.path.contains("output.txt"));
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn json_to_resolved_value_relative_file() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        // Create a file so FileValue::from_path can stat it
        std::fs::write(workdir.join("result.dat"), "data").unwrap();

        let v = json_to_resolved_value(
            &serde_json::json!({
                "class": "File",
                "path": "result.dat"
            }),
            workdir,
        );
        match v {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "result.dat");
                assert!(fv.size > 0);
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn json_to_resolved_value_directory_object() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        let sub = workdir.join("outdir");
        std::fs::create_dir_all(&sub).unwrap();

        let v = json_to_resolved_value(
            &serde_json::json!({
                "class": "Directory",
                "path": sub.to_str().unwrap()
            }),
            workdir,
        );
        match v {
            ResolvedValue::Directory(fv) => {
                assert_eq!(fv.basename, "outdir");
            }
            other => panic!("expected Directory, got {:?}", other),
        }
    }

    #[test]
    fn json_to_resolved_value_file_with_location_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path();

        let test_file = workdir.join("hello.txt");
        std::fs::write(&test_file, "hi").unwrap();

        let location = format!("file://{}", test_file.to_str().unwrap());
        let v = json_to_resolved_value(
            &serde_json::json!({
                "class": "File",
                "location": location
            }),
            workdir,
        );
        match v {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.basename, "hello.txt");
                assert!(fv.size > 0);
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn json_to_resolved_value_generic_object() {
        let workdir = std::path::Path::new("/tmp/test_workdir");

        let v = json_to_resolved_value(
            &serde_json::json!({"key": "value", "num": 1}),
            workdir,
        );
        match v {
            ResolvedValue::String(s) => {
                assert!(s.contains("key"));
                assert!(s.contains("value"));
            }
            other => panic!("expected String (serialized JSON), got {:?}", other),
        }
    }

    #[test]
    fn cwl_output_json_integration() {
        // End-to-end: simulate cwl.output.json being present and parsed
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();

        let output_json = serde_json::json!({
            "args": ["echo", "hello", "world"]
        });
        std::fs::write(
            workdir.join("cwl.output.json"),
            serde_json::to_string(&output_json).unwrap(),
        )
        .unwrap();

        // Verify the file is readable and parseable
        let json_str = std::fs::read_to_string(workdir.join("cwl.output.json")).unwrap();
        let json_val: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        if let serde_json::Value::Object(map) = json_val {
            let mut outputs = HashMap::new();
            for (key, val) in map {
                outputs.insert(key, json_to_resolved_value(&val, &workdir));
            }
            assert!(outputs.contains_key("args"));
            match outputs.get("args") {
                Some(ResolvedValue::Array(arr)) => {
                    assert_eq!(arr.len(), 3);
                    assert!(matches!(&arr[0], ResolvedValue::String(s) if s == "echo"));
                    assert!(matches!(&arr[1], ResolvedValue::String(s) if s == "hello"));
                    assert!(matches!(&arr[2], ResolvedValue::String(s) if s == "world"));
                }
                other => panic!("expected Array, got {:?}", other),
            }
        } else {
            panic!("expected JSON object");
        }
    }

    // -- Test: yaml_to_resolved handles File objects ----------------------------

    #[test]
    fn yaml_to_resolved_file_object() {
        let yaml_val: serde_yaml::Value = serde_yaml::from_str(
            r#"
class: File
location: args.py
"#,
        )
        .unwrap();
        let resolved = yaml_to_resolved(&yaml_val);
        match resolved {
            ResolvedValue::File(fv) => {
                assert_eq!(fv.path, "args.py");
                assert_eq!(fv.basename, "args.py");
            }
            other => panic!("expected File, got {:?}", other),
        }
    }

    #[test]
    fn yaml_to_resolved_directory_object() {
        let yaml_val: serde_yaml::Value = serde_yaml::from_str(
            r#"
class: Directory
path: /data/outdir
"#,
        )
        .unwrap();
        let resolved = yaml_to_resolved(&yaml_val);
        match resolved {
            ResolvedValue::Directory(fv) => {
                assert_eq!(fv.path, "/data/outdir");
            }
            other => panic!("expected Directory, got {:?}", other),
        }
    }

    // -- ExpressionTool execution tests -----------------------------------------

    #[test]
    fn execute_expression_tool_simple_passthrough() {
        let expr_tool = ExpressionTool {
            cwl_version: None,
            label: None,
            doc: None,
            requirements: Vec::new(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            expression: Some("${return {\"output\": inputs.message};}".to_string()),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "message".to_string(),
            ResolvedValue::String("hello world".to_string()),
        );

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let result = execute_expression_tool(&expr_tool, &inputs, &runtime).unwrap();
        match result.get("output") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "hello world"),
            other => panic!("expected String(\"hello world\"), got {:?}", other),
        }
    }

    #[test]
    fn execute_expression_tool_parse_int() {
        let expr_tool = ExpressionTool {
            cwl_version: None,
            label: None,
            doc: None,
            requirements: Vec::new(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            expression: Some("${return {\"output\": parseInt(inputs.file1.contents)};}".to_string()),
        };

        let mut inputs = HashMap::new();
        inputs.insert(
            "file1".to_string(),
            ResolvedValue::File(FileValue {
                path: "/data/count.txt".to_string(),
                basename: "count.txt".to_string(),
                nameroot: "count".to_string(),
                nameext: ".txt".to_string(),
                size: 3,
                checksum: None,
                secondary_files: Vec::new(),
                contents: Some("  16\n".to_string()),
            }),
        );

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let result = execute_expression_tool(&expr_tool, &inputs, &runtime).unwrap();
        match result.get("output") {
            Some(ResolvedValue::Int(n)) => assert_eq!(*n, 16),
            other => panic!("expected Int(16), got {:?}", other),
        }
    }

    #[test]
    fn execute_expression_tool_no_expression() {
        let expr_tool = ExpressionTool {
            cwl_version: None,
            label: None,
            doc: None,
            requirements: Vec::new(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            expression: None,
        };

        let inputs = HashMap::new();
        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let result = execute_expression_tool(&expr_tool, &inputs, &runtime);
        assert!(result.is_err());
    }

    #[test]
    fn execute_expression_tool_multiple_outputs() {
        let expr_tool = ExpressionTool {
            cwl_version: None,
            label: None,
            doc: None,
            requirements: Vec::new(),
            inputs: HashMap::new(),
            outputs: HashMap::new(),
            expression: Some(
                "${return {\"count\": parseInt(inputs.val), \"name\": inputs.label};}".to_string(),
            ),
        };

        let mut inputs = HashMap::new();
        inputs.insert("val".to_string(), ResolvedValue::String("42".to_string()));
        inputs.insert(
            "label".to_string(),
            ResolvedValue::String("test".to_string()),
        );

        let runtime = RuntimeContext {
            cores: 1,
            ram: 1024,
            outdir: "/tmp/out".to_string(),
            tmpdir: "/tmp/tmp".to_string(),
        };

        let result = execute_expression_tool(&expr_tool, &inputs, &runtime).unwrap();
        match result.get("count") {
            Some(ResolvedValue::Int(n)) => assert_eq!(*n, 42),
            other => panic!("expected Int(42), got {:?}", other),
        }
        match result.get("name") {
            Some(ResolvedValue::String(s)) => assert_eq!(s, "test"),
            other => panic!("expected String(\"test\"), got {:?}", other),
        }
    }
}
