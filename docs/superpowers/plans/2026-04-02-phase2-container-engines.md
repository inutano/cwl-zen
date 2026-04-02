# Phase 2: Multi-Engine Container Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support Singularity, Apptainer, and Podman alongside Docker with auto-detection, SIF caching, symlink staging, auto-retry, and bundled bug fixes.

**Architecture:** Extract container logic behind a `ContainerEngine` trait with four implementations (two code paths: OCI for Docker/Podman, SIF for Singularity/Apptainer). Auto-detect available runtime. Add symlink-based input staging with automatic copy-retry on failure. Fix known Phase 1 bugs.

**Tech Stack:** Rust, std::process::Command, std::os::unix::fs::symlink

**Specs:**
- `docs/superpowers/specs/2026-04-02-phase2-container-engines-design.md`
- Current codebase: 4,445 lines, 65 tests, 10 modules

---

## File Structure

```
src/
  container.rs    — NEW: ContainerEngine trait, 4 implementations, auto-detection, SIF caching
  staging.rs      — NEW: symlink/copy input staging, auto-retry logic
  execute.rs      — MODIFY: use ContainerEngine + staging instead of direct Docker commands
  command.rs      — MODIFY: remove docker_image field, fix separate/shellQuote
  stage.rs        — MODIFY: remove build_docker_mounts (moved to container.rs)
  main.rs         — MODIFY: add --engine, --container-cache, --copy-inputs, --no-retry-copy flags
  parse.rs        — MODIFY: add has_network_access(), has_initial_workdir_requirement(), writable_inputs()
  provenance.rs   — MODIFY: fix dateModified timestamp
  input.rs        — MODIFY: fix file:// URI stripping
  model.rs        — no changes needed
  lib.rs          — MODIFY: add pub mod container; pub mod staging;
tests/
  fixtures/
    write-next-to-input.cwl  — NEW: tool that writes next to input (for copy-retry test)
```

---

## Task 1: Container Engine Trait and OCI Implementation (Docker/Podman)

**Files:**
- Create: `src/container.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write tests for OCI engine mount generation and SIF cache paths**

Add to `src/container.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn oci_mount_format() {
        let m = Mount { source: PathBuf::from("/data"), target: PathBuf::from("/data"), readonly: true };
        let s = format_oci_mount(&m);
        assert_eq!(s, "--mount=type=bind,source=/data,target=/data,readonly");
    }

    #[test]
    fn oci_mount_readwrite() {
        let m = Mount { source: PathBuf::from("/work"), target: PathBuf::from("/work"), readonly: false };
        let s = format_oci_mount(&m);
        assert_eq!(s, "--mount=type=bind,source=/work,target=/work");
    }

    #[test]
    fn sif_mount_format() {
        let m = Mount { source: PathBuf::from("/data"), target: PathBuf::from("/data"), readonly: true };
        let s = format_sif_mount(&m);
        assert_eq!(s, "--bind=/data:/data:ro");
    }

    #[test]
    fn sif_mount_readwrite() {
        let m = Mount { source: PathBuf::from("/work"), target: PathBuf::from("/work"), readonly: false };
        let s = format_sif_mount(&m);
        assert_eq!(s, "--bind=/work:/work");
    }

    #[test]
    fn sif_cache_path_derivation() {
        let cache = PathBuf::from("/home/user/.cwl-zen/containers");
        let path = sif_cache_path("quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8", &cache);
        assert_eq!(path, PathBuf::from("/home/user/.cwl-zen/containers/quay.io/biocontainers/bwa-mem2_2.2.1--he70b90d_8.sif"));
    }

    #[test]
    fn sif_cache_path_dockerhub() {
        let cache = PathBuf::from("/cache");
        let path = sif_cache_path("ubuntu:22.04", &cache);
        assert_eq!(path, PathBuf::from("/cache/docker.io/library/ubuntu_22.04.sif"));
    }

    #[test]
    fn detect_priority_order() {
        // This test verifies the priority list is correct (podman > apptainer > singularity > docker)
        let order = engine_priority_order();
        assert_eq!(order, vec!["podman", "apptainer", "singularity", "docker"]);
    }

    #[test]
    fn resolve_cache_dir_default() {
        // When no env var or flag, uses ~/.cwl-zen/containers/
        std::env::remove_var("CWL_ZEN_CONTAINER_CACHE");
        let dir = resolve_container_cache(None);
        assert!(dir.to_string_lossy().ends_with(".cwl-zen/containers"));
    }

    #[test]
    fn resolve_cache_dir_env() {
        std::env::set_var("CWL_ZEN_CONTAINER_CACHE", "/shared/sif");
        let dir = resolve_container_cache(None);
        assert_eq!(dir, PathBuf::from("/shared/sif"));
        std::env::remove_var("CWL_ZEN_CONTAINER_CACHE");
    }

    #[test]
    fn resolve_cache_dir_explicit() {
        std::env::set_var("CWL_ZEN_CONTAINER_CACHE", "/shared/sif");
        let dir = resolve_container_cache(Some(Path::new("/explicit")));
        assert_eq!(dir, PathBuf::from("/explicit"));
        std::env::remove_var("CWL_ZEN_CONTAINER_CACHE");
    }
}
```

- [ ] **Step 2: Implement `src/container.rs`**

```rust
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{bail, Context, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub struct ContainerExecRequest {
    pub image: String,
    pub command: String,
    pub use_shell: bool,
    pub workdir: PathBuf,
    pub mounts: Vec<Mount>,
    pub network: bool,
    pub cores: u32,
    pub ram: u64,
    pub stdout: std::fs::File,
    pub stderr: std::fs::File,
}

pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub readonly: bool,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

pub trait ContainerEngine {
    fn name(&self) -> &str;
    fn pull(&self, image: &str, cache_dir: &Path) -> Result<()>;
    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus>;
}

// ---------------------------------------------------------------------------
// OCI engines (Docker, Podman)
// ---------------------------------------------------------------------------

pub struct OciEngine {
    binary: String, // "docker" or "podman"
}

impl OciEngine {
    pub fn docker() -> Self { Self { binary: "docker".into() } }
    pub fn podman() -> Self { Self { binary: "podman".into() } }
}

impl ContainerEngine for OciEngine {
    fn name(&self) -> &str { &self.binary }

    fn pull(&self, _image: &str, _cache_dir: &Path) -> Result<()> {
        // OCI engines handle pulling internally during `run`
        Ok(())
    }

    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("run").arg("--rm");
        cmd.arg("--workdir=/work");

        for mount in &req.mounts {
            cmd.arg(format_oci_mount(mount));
        }

        if !req.network {
            cmd.arg("--network=none");
        }

        cmd.arg(&req.image);

        if req.use_shell {
            cmd.args(["sh", "-c", &req.command]);
        } else {
            for part in req.command.split_whitespace() {
                cmd.arg(part);
            }
        }

        cmd.stdout(req.stdout).stderr(req.stderr);

        let status = cmd.status()
            .with_context(|| format!("{}: failed to execute container", self.binary))?;
        Ok(status)
    }
}

fn format_oci_mount(m: &Mount) -> String {
    let base = format!(
        "--mount=type=bind,source={},target={}",
        m.source.display(),
        m.target.display()
    );
    if m.readonly { format!("{base},readonly") } else { base }
}

// ---------------------------------------------------------------------------
// SIF engines (Singularity, Apptainer)
// ---------------------------------------------------------------------------

pub struct SifEngine {
    binary: String, // "singularity" or "apptainer"
}

impl SifEngine {
    pub fn singularity() -> Self { Self { binary: "singularity".into() } }
    pub fn apptainer() -> Self { Self { binary: "apptainer".into() } }
}

impl ContainerEngine for SifEngine {
    fn name(&self) -> &str { &self.binary }

    fn pull(&self, image: &str, cache_dir: &Path) -> Result<()> {
        let sif_path = sif_cache_path(image, cache_dir);
        if sif_path.exists() {
            return Ok(());
        }
        // Create parent directories
        if let Some(parent) = sif_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        eprintln!("Pulling image: {image} -> {}", sif_path.display());
        let status = Command::new(&self.binary)
            .args(["pull", &sif_path.to_string_lossy(), &format!("docker://{image}")])
            .status()
            .with_context(|| format!("{}: failed to pull {image}", self.binary))?;
        if !status.success() {
            bail!(
                "Failed to pull image: {image}\nRun manually: {} pull docker://{image}",
                self.binary
            );
        }
        Ok(())
    }

    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus> {
        let cache_dir = resolve_container_cache(None);
        let sif_path = sif_cache_path(&req.image, &cache_dir);
        if !sif_path.exists() {
            bail!(
                "SIF image not found: {}. Run: {} pull docker://{}",
                sif_path.display(), self.binary, req.image
            );
        }

        let mut cmd = Command::new(&self.binary);
        cmd.arg("exec");
        cmd.arg(format!("--pwd=/work"));

        for mount in &req.mounts {
            cmd.arg(format_sif_mount(mount));
        }

        // Singularity doesn't isolate network by default; no --network flag needed
        // unless we specifically want isolation

        cmd.arg(&sif_path);

        if req.use_shell {
            cmd.args(["sh", "-c", &req.command]);
        } else {
            for part in req.command.split_whitespace() {
                cmd.arg(part);
            }
        }

        cmd.stdout(req.stdout).stderr(req.stderr);

        let status = cmd.status()
            .with_context(|| format!("{}: failed to exec container", self.binary))?;
        Ok(status)
    }
}

fn format_sif_mount(m: &Mount) -> String {
    let bind = format!("--bind={}:{}", m.source.display(), m.target.display());
    if m.readonly { format!("{bind}:ro") } else { bind }
}

// ---------------------------------------------------------------------------
// SIF cache path
// ---------------------------------------------------------------------------

/// Derive the local SIF file path from a Docker image URI.
/// `quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8`
///   → `{cache}/quay.io/biocontainers/bwa-mem2_2.2.1--he70b90d_8.sif`
/// Bare images like `ubuntu:22.04` get `docker.io/library/` prefix.
pub fn sif_cache_path(image: &str, cache_dir: &Path) -> PathBuf {
    let normalized = if !image.contains('/') {
        // Bare image like "ubuntu:22.04" → "docker.io/library/ubuntu:22.04"
        format!("docker.io/library/{image}")
    } else if image.matches('/').count() == 1 && !image.contains('.') {
        // Docker Hub user image like "user/repo:tag" → "docker.io/user/repo:tag"
        format!("docker.io/{image}")
    } else {
        image.to_string()
    };

    // Split into directory path and filename
    let last_slash = normalized.rfind('/').unwrap();
    let dir_part = &normalized[..last_slash];
    let name_part = &normalized[last_slash + 1..];

    // Replace : with _ in filename, add .sif
    let filename = format!("{}.sif", name_part.replace(':', "_"));

    cache_dir.join(dir_part).join(filename)
}

// ---------------------------------------------------------------------------
// Auto-detection
// ---------------------------------------------------------------------------

/// Return the engine priority order.
pub fn engine_priority_order() -> Vec<&'static str> {
    vec!["podman", "apptainer", "singularity", "docker"]
}

fn is_on_path(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Auto-detect the best available container engine.
pub fn detect_engine() -> Result<Box<dyn ContainerEngine>> {
    for name in engine_priority_order() {
        if is_on_path(name) {
            return engine_by_name(name);
        }
    }
    bail!("No container runtime found. Install one of: podman, apptainer, singularity, docker")
}

/// Get a container engine by name.
pub fn engine_by_name(name: &str) -> Result<Box<dyn ContainerEngine>> {
    match name {
        "docker" => Ok(Box::new(OciEngine::docker())),
        "podman" => Ok(Box::new(OciEngine::podman())),
        "singularity" => Ok(Box::new(SifEngine::singularity())),
        "apptainer" => Ok(Box::new(SifEngine::apptainer())),
        _ => bail!("Unknown container engine: {name}. Valid: docker, podman, singularity, apptainer"),
    }
}

// ---------------------------------------------------------------------------
// Cache directory resolution
// ---------------------------------------------------------------------------

/// Resolve the container cache directory.
/// Priority: explicit flag > $CWL_ZEN_CONTAINER_CACHE > ~/.cwl-zen/containers/
pub fn resolve_container_cache(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(val) = std::env::var("CWL_ZEN_CONTAINER_CACHE") {
        return PathBuf::from(val);
    }
    dirs_or_home().join(".cwl-zen").join("containers")
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ---------------------------------------------------------------------------
// Helper: build mounts from resolved inputs
// ---------------------------------------------------------------------------

/// Build mount list from resolved inputs and a workdir.
pub fn build_mounts(
    inputs: &std::collections::HashMap<String, crate::model::ResolvedValue>,
    workdir: &Path,
) -> Vec<Mount> {
    use crate::model::ResolvedValue;
    let mut mounts = Vec::new();
    let mut seen_dirs = std::collections::HashSet::new();

    // Mount workdir
    let workdir_abs = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    mounts.push(Mount {
        source: workdir_abs.clone(),
        target: PathBuf::from("/work"),
        readonly: false,
    });
    seen_dirs.insert(workdir_abs);

    fn collect_mounts(
        val: &ResolvedValue,
        mounts: &mut Vec<Mount>,
        seen: &mut std::collections::HashSet<PathBuf>,
    ) {
        match val {
            ResolvedValue::File(f) => {
                if let Some(parent) = f.path.parent() {
                    let abs = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
                    if seen.insert(abs.clone()) {
                        mounts.push(Mount {
                            source: abs.clone(),
                            target: abs,
                            readonly: true,
                        });
                    }
                }
                for sf in &f.secondary_files {
                    collect_mounts(&ResolvedValue::File(sf.clone()), mounts, seen);
                }
            }
            ResolvedValue::Array(arr) => {
                for v in arr { collect_mounts(v, mounts, seen); }
            }
            _ => {}
        }
    }

    for val in inputs.values() {
        collect_mounts(val, &mut mounts, &mut seen_dirs);
    }

    mounts
}
```

- [ ] **Step 3: Update `src/lib.rs`**

Add `pub mod container;`

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test container
```

Expected: all container tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/container.rs src/lib.rs
git commit -m "feat: container engine trait with Docker, Podman, Singularity, Apptainer support"
```

---

## Task 2: Input Staging Module

**Files:**
- Create: `src/staging.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn symlink_staging_creates_links() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();

        // Create a fake input file
        let input_dir = dir.path().join("inputs");
        std::fs::create_dir_all(&input_dir).unwrap();
        let input_file = input_dir.join("sample.bam");
        std::fs::write(&input_file, b"fake bam").unwrap();

        let fv = crate::model::FileValue::from_path(&input_file);
        let mut inputs = HashMap::new();
        inputs.insert("bam".to_string(), ResolvedValue::File(fv));

        let staged = stage_inputs(&inputs, &workdir, StagingMode::Symlink).unwrap();

        // Check symlink exists
        let link = workdir.join("sample.bam");
        assert!(link.exists());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        // Check staged inputs point to workdir
        match staged.get("bam").unwrap() {
            ResolvedValue::File(f) => {
                assert_eq!(f.path, link);
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn copy_staging_creates_copies() {
        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path().join("work");
        std::fs::create_dir_all(&workdir).unwrap();

        let input_dir = dir.path().join("inputs");
        std::fs::create_dir_all(&input_dir).unwrap();
        let input_file = input_dir.join("sample.bam");
        std::fs::write(&input_file, b"fake bam").unwrap();

        let fv = crate::model::FileValue::from_path(&input_file);
        let mut inputs = HashMap::new();
        inputs.insert("bam".to_string(), ResolvedValue::File(fv));

        let staged = stage_inputs(&inputs, &workdir, StagingMode::Copy).unwrap();

        let copy = workdir.join("sample.bam");
        assert!(copy.exists());
        assert!(!copy.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&copy).unwrap(), b"fake bam");
    }

    #[test]
    fn is_symlink_error_detected() {
        assert!(is_symlink_error("Error: Read-only file system"));
        assert!(is_symlink_error("Too many levels of symbolic links"));
        assert!(is_symlink_error("some Permission denied here"));
        assert!(!is_symlink_error("normal error message"));
    }
}
```

- [ ] **Step 2: Implement `src/staging.rs`**

```rust
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{FileValue, ResolvedValue};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StagingMode {
    Symlink,
    Copy,
}

/// Stage input files into the workdir.
/// Returns a new inputs map with file paths pointing into the workdir.
pub fn stage_inputs(
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    mode: StagingMode,
) -> Result<HashMap<String, ResolvedValue>> {
    let mut staged = HashMap::new();
    for (name, val) in inputs {
        staged.insert(name.clone(), stage_value(val, workdir, mode)?);
    }
    Ok(staged)
}

fn stage_value(val: &ResolvedValue, workdir: &Path, mode: StagingMode) -> Result<ResolvedValue> {
    match val {
        ResolvedValue::File(f) => {
            let staged_path = workdir.join(&f.basename);
            if f.path.exists() && !staged_path.exists() {
                match mode {
                    StagingMode::Symlink => {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(&f.path, &staged_path)
                            .with_context(|| format!(
                                "symlinking {} -> {}",
                                f.path.display(), staged_path.display()
                            ))?;
                    }
                    StagingMode::Copy => {
                        std::fs::copy(&f.path, &staged_path)
                            .with_context(|| format!(
                                "copying {} -> {}",
                                f.path.display(), staged_path.display()
                            ))?;
                    }
                }
            }
            let mut staged_fv = FileValue::from_path(&staged_path);
            // Stage secondary files too
            for sf in &f.secondary_files {
                if let ResolvedValue::File(sf_staged) = stage_value(&ResolvedValue::File(sf.clone()), workdir, mode)? {
                    staged_fv.secondary_files.push(sf_staged);
                }
            }
            Ok(ResolvedValue::File(staged_fv))
        }
        ResolvedValue::Array(arr) => {
            let staged: Result<Vec<_>> = arr.iter().map(|v| stage_value(v, workdir, mode)).collect();
            Ok(ResolvedValue::Array(staged?))
        }
        other => Ok(other.clone()),
    }
}

/// Check if an error message suggests symlink-related failure.
pub fn is_symlink_error(stderr_content: &str) -> bool {
    let patterns = [
        "Read-only file system",
        "Too many levels of symbolic links",
        "Operation not permitted",
        "Permission denied",
    ];
    patterns.iter().any(|p| stderr_content.contains(p))
}
```

- [ ] **Step 3: Update `src/lib.rs`**

Add `pub mod staging;`

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test staging
```

Expected: all staging tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/staging.rs src/lib.rs
git commit -m "feat: input staging module with symlink and copy modes"
```

---

## Task 3: Integrate Container Engine into Executor

**Files:**
- Modify: `src/execute.rs`

- [ ] **Step 1: Refactor `execute_tool` to use ContainerEngine**

Replace the Docker-specific code in `execute_tool()` (lines 86-147) with:

```rust
pub fn execute_tool(
    tool: &crate::model::CommandLineTool,
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
    runtime: &RuntimeContext,
    log_dir: &Path,
    step_name: &str,
    engine: &dyn crate::container::ContainerEngine,
    staging_mode: crate::staging::StagingMode,
) -> Result<(i32, HashMap<String, ResolvedValue>)>
```

The function now:
1. Creates workdir and log_dir
2. Stages inputs into workdir via `staging::stage_inputs()`
3. Builds command via `command::build_command()` (using staged inputs)
4. If docker_image is set:
   - Pull image via `engine.pull()`
   - Build mounts via `container::build_mounts()`
   - Execute via `engine.exec()`
5. If no docker_image: run directly (unchanged)
6. If step fails and `staging_mode == Symlink`: check stderr for symlink errors and auto-retry with Copy
7. Handle stdout redirect, collect outputs

Key change: the auto-retry logic:

```rust
let exit_code = run_step(engine, &resolved_cmd, &staged_inputs, workdir, &stdout_log, &stderr_log)?;

if exit_code != 0 && staging_mode == StagingMode::Symlink {
    let stderr_content = std::fs::read_to_string(&stderr_log).unwrap_or_default();
    if staging::is_symlink_error(&stderr_content) {
        eprintln!("Step '{}' failed. Retrying with copy-staged inputs...", step_name);
        // Re-create workdir with copied inputs
        let copy_workdir = workdir.with_extension("copy");
        std::fs::create_dir_all(&copy_workdir)?;
        let copy_staged = staging::stage_inputs(inputs, &copy_workdir, StagingMode::Copy)?;
        let retry_code = run_step(engine, &resolved_cmd, &copy_staged, &copy_workdir, &stdout_log, &stderr_log)?;
        if retry_code == 0 {
            eprintln!("Step '{}' succeeded after copy-staging.", step_name);
        }
        // Use copy_workdir for output collection
        return collect_and_return(tool, &copy_staged, runtime, &copy_workdir, retry_code);
    }
}
```

- [ ] **Step 2: Update `execute_workflow` signature**

```rust
pub fn execute_workflow(
    workflow_path: &Path,
    workflow: &Workflow,
    dag: &[DagStep],
    inputs: &HashMap<String, ResolvedValue>,
    outdir: &Path,
    engine: &dyn crate::container::ContainerEngine,
    staging_mode: crate::staging::StagingMode,
) -> Result<RunResult>
```

Pass `engine` and `staging_mode` through to `execute_tool()`.

- [ ] **Step 3: Apply bug fix — merge workflow defaults**

At the start of `execute_workflow`, before the step loop:

```rust
let mut inputs = inputs.clone();
for (name, wf_input) in &workflow.inputs {
    if !inputs.contains_key(name) {
        if let Some(default) = &wf_input.default {
            inputs.insert(name.clone(), yaml_to_resolved(default));
        }
    }
}
```

- [ ] **Step 4: Apply bug fix — pass wf_inputs to resolve_step_inputs**

In `resolve_step_inputs`, when handling `value_from`:

```rust
StepInput::Structured(entry) => {
    // ...
    if let Some(vf) = &entry.value_from {
        let s = crate::param::resolve_param_refs(vf, wf_inputs, runtime, Some(&base));
        ResolvedValue::String(s)
    }
}
```

Change the empty `&HashMap::new()` to `wf_inputs`.

- [ ] **Step 5: Update existing tests to pass engine and staging_mode**

For unit tests in execute.rs, create a helper:

```rust
#[cfg(test)]
fn no_container_engine() -> Box<dyn crate::container::ContainerEngine> {
    // Tests that don't use containers can use a dummy
    Box::new(crate::container::OciEngine::docker())
}
```

Update test calls to include the new parameters.

- [ ] **Step 6: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

Expected: all tests pass (some may need updating due to signature changes).

- [ ] **Step 7: Commit**

```bash
git add src/execute.rs
git commit -m "feat: integrate container engine and staging into executor"
```

---

## Task 4: Update Command Builder — Remove Docker, Fix Bugs

**Files:**
- Modify: `src/command.rs`

- [ ] **Step 1: Remove `docker_image` from ResolvedCommand**

The docker image is now handled by the container engine. `ResolvedCommand` becomes:

```rust
pub struct ResolvedCommand {
    pub command_line: String,
    pub use_shell: bool,
    pub cores: u32,
    pub ram: u64,
    pub stdout_file: Option<String>,
    pub docker_image: Option<String>,  // Keep for reference, but execution uses ContainerEngine
    pub network_access: bool,          // NEW: from NetworkAccess requirement
}
```

- [ ] **Step 2: Fix `inputBinding.separate`**

In the input binding section of `build_command()`, change prefix formatting:

```rust
if let Some(prefix) = &binding.prefix {
    if binding.separate.unwrap_or(true) {
        bound_inputs.push((pos, format!("{} {}", prefix, val)));
    } else {
        bound_inputs.push((pos, format!("{}{}", prefix, val)));
    }
}
```

- [ ] **Step 3: Add `network_access` detection**

Add a helper to `parse.rs`:

```rust
pub fn has_network_access(tool: &CommandLineTool) -> bool {
    tool.requirements.iter().any(|req| {
        req.get("class").and_then(|v| v.as_str()) == Some("NetworkAccess")
    })
}
```

Use it in `build_command()`:

```rust
let network_access = crate::parse::has_network_access(tool);
```

- [ ] **Step 4: Add tests for separate and network_access**

```rust
#[test]
fn separate_false_no_space() {
    let doc = crate::parse::parse_cwl_str(r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: tool
inputs:
  threads:
    type: int
    inputBinding:
      prefix: --threads=
      separate: false
      position: 1
outputs: {}
"#).unwrap();
    let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
    let mut inputs = HashMap::new();
    inputs.insert("threads".into(), ResolvedValue::Int(8));
    let cmd = build_command(&tool, &inputs, &basic_runtime());
    assert!(cmd.command_line.contains("--threads=8"));
    assert!(!cmd.command_line.contains("--threads= 8"));
}

#[test]
fn network_access_detected() {
    let doc = crate::parse::parse_cwl_str(r#"
cwlVersion: v1.2
class: CommandLineTool
baseCommand: curl
requirements:
  - class: NetworkAccess
    networkAccess: true
inputs: {}
outputs: {}
"#).unwrap();
    let tool = match doc { CwlDocument::CommandLineTool(t) => t, _ => panic!() };
    let cmd = build_command(&tool, &HashMap::new(), &basic_runtime());
    assert!(cmd.network_access);
}
```

- [ ] **Step 5: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test command
```

- [ ] **Step 6: Commit**

```bash
git add src/command.rs src/parse.rs
git commit -m "fix: inputBinding.separate, add NetworkAccess detection"
```

---

## Task 5: Update CLI with New Flags

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add new CLI flags**

```rust
#[derive(Subcommand)]
enum Commands {
    Run {
        cwl_file: PathBuf,
        input_file: PathBuf,

        #[arg(long, default_value = "./cwl-zen-output")]
        outdir: PathBuf,

        #[arg(long)]
        no_crate: bool,

        /// Container engine override (auto-detected if not set)
        #[arg(long, value_parser = ["docker", "podman", "singularity", "apptainer"])]
        engine: Option<String>,

        /// Container image cache directory
        #[arg(long)]
        container_cache: Option<PathBuf>,

        /// Force copy-staging for all inputs (instead of symlinks)
        #[arg(long)]
        copy_inputs: bool,

        /// Disable automatic retry with copy-staging on symlink failures
        #[arg(long)]
        no_retry_copy: bool,
    },
    // ... Validate and Dag unchanged
}
```

- [ ] **Step 2: Update `cmd_run` to use engine detection**

```rust
fn cmd_run(
    cwl_file: &Path,
    input_file: &Path,
    outdir: &Path,
    no_crate: bool,
    engine_name: Option<&str>,
    container_cache: Option<&Path>,
    copy_inputs: bool,
    no_retry_copy: bool,
) {
    // Detect or select engine
    let engine: Box<dyn cwl_zen::container::ContainerEngine> = match engine_name {
        Some(name) => {
            if !cwl_zen::container::is_on_path(name) {
                eprintln!("Error: container engine '{name}' not found on PATH");
                process::exit(1);
            }
            cwl_zen::container::engine_by_name(name).unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                process::exit(1);
            })
        }
        None => cwl_zen::container::detect_engine().unwrap_or_else(|e| {
            eprintln!("Warning: {e}");
            eprintln!("Running without container support (tools must be on PATH)");
            // Fall back to a no-op engine or direct execution
            // For now, use docker as fallback
            Box::new(cwl_zen::container::OciEngine::docker())
        }),
    };

    eprintln!("Container engine: {}", engine.name());

    let staging_mode = if copy_inputs {
        cwl_zen::staging::StagingMode::Copy
    } else if no_retry_copy {
        cwl_zen::staging::StagingMode::Symlink  // no retry
    } else {
        cwl_zen::staging::StagingMode::Symlink  // with auto-retry (handled in execute)
    };

    // ... rest of cmd_run passes engine and staging_mode to execute functions
}
```

- [ ] **Step 3: Build and verify**

```bash
cd ~/repos/cwl-zen && cargo build
cd ~/repos/cwl-zen && cargo run -- run --help
```

Expected: new flags visible in help output.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add --engine, --container-cache, --copy-inputs, --no-retry-copy flags"
```

---

## Task 6: Bug Fixes — Provenance dateModified + Input file:// URI

**Files:**
- Modify: `src/provenance.rs`
- Modify: `src/input.rs`

- [ ] **Step 1: Fix dateModified in provenance.rs**

Find the `file_entity()` function (or equivalent) that creates File entities. Replace `Utc::now()` with actual file modification time:

```rust
// Instead of: format_timestamp(&Utc::now())
// Use:
if file.path.exists() {
    if let Ok(metadata) = std::fs::metadata(&file.path) {
        if let Ok(modified) = metadata.modified() {
            let dt: DateTime<Utc> = modified.into();
            entity["dateModified"] = json!(format_timestamp(&dt));
        }
    }
}
```

- [ ] **Step 2: Fix file:// URI in input.rs**

Find the function that resolves file paths from input YAML. Add URI stripping:

```rust
// After extracting the raw path string:
let raw_path = raw_path.strip_prefix("file://").unwrap_or(raw_path);
```

- [ ] **Step 3: Add tests**

In `src/input.rs` tests:
```rust
#[test]
fn parse_file_uri() {
    let yaml = r#"
input_file:
  class: File
  location: "file:///data/sample.bam"
"#;
    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("input.yml");
    std::fs::write(&input_path, yaml).unwrap();
    let inputs = parse_inputs(&input_path, dir.path()).unwrap();
    match inputs.get("input_file") {
        Some(ResolvedValue::File(f)) => {
            assert!(f.path.to_string_lossy().contains("/data/sample.bam"));
            assert!(!f.path.to_string_lossy().contains("file://"));
        }
        other => panic!("expected File, got {:?}", other),
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

- [ ] **Step 5: Commit**

```bash
git add src/provenance.rs src/input.rs
git commit -m "fix: dateModified uses actual file mtime, strip file:// URIs in input parser"
```

---

## Task 7: Remove Old Docker Mounts from stage.rs

**Files:**
- Modify: `src/stage.rs`

- [ ] **Step 1: Remove `build_docker_mounts` function**

This function is now replaced by `container::build_mounts()`. Remove the function and its tests. Update any remaining references in other modules.

- [ ] **Step 2: Verify no other code calls `build_docker_mounts`**

```bash
cd ~/repos/cwl-zen && grep -rn "build_docker_mounts" src/
```

If found in execute.rs, it should have been replaced in Task 3. Fix any remaining references.

- [ ] **Step 3: Run tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

- [ ] **Step 4: Commit**

```bash
git add src/stage.rs
git commit -m "refactor: remove build_docker_mounts (replaced by container::build_mounts)"
```

---

## Task 8: Update Integration Tests

**Files:**
- Modify: `tests/integration_echo.rs`
- Modify: `tests/integration_workflow.rs`

- [ ] **Step 1: Update integration tests for new execute_tool/execute_workflow signatures**

Both integration tests need to:
1. Detect a container engine (or use direct execution for echo/cat which don't need containers)
2. Pass `engine` and `staging_mode` to the execute functions

For echo and cat tools (no Docker required), the tests should work without a container engine. Update the test to handle the new signatures.

```rust
// In integration_echo.rs:
use cwl_zen::staging::StagingMode;

#[test]
fn run_echo_tool() {
    // ... existing setup ...

    // For tools without DockerRequirement, engine is not used
    // but we still need to pass one
    let engine = cwl_zen::container::OciEngine::docker();

    let (exit_code, _outputs) = execute::execute_tool(
        &tool, &inputs, workdir, &runtime, &log_dir, "echo",
        &engine, StagingMode::Symlink,
    ).unwrap();
    assert_eq!(exit_code, 0);
    // ... rest of assertions
}
```

- [ ] **Step 2: Run all tests**

```bash
cd ~/repos/cwl-zen && cargo test
```

- [ ] **Step 3: Commit**

```bash
git add tests/
git commit -m "test: update integration tests for container engine and staging signatures"
```

---

## Task 9: Update Spec and README

**Files:**
- Modify: `docs/spec.md`
- Modify: `README.md`

- [ ] **Step 1: Update spec.md**

Add to the Runner Design section:

```markdown
### Container engines

cwl-zen auto-detects the available container runtime in this priority order:
podman → apptainer → singularity → docker

Override with `--engine`:
```bash
cwl-zen run --engine singularity workflow.cwl input.yml
```

SIF images for Singularity/Apptainer are cached in `~/.cwl-zen/containers/`.
Override with `--container-cache` or `$CWL_ZEN_CONTAINER_CACHE`.
```

Move `InitialWorkDirRequirement` from "Not Supported" to a new row:

```markdown
| `InitialWorkDirRequirement` (minimal) | `writable: true` only | Copy staging for specific inputs |
```

Add the philosophy update about scatter:

```markdown
### Execution model

One workflow, one input, one run. Batching across samples is the job of the
caller: an AI agent, a SLURM array job, a bash loop. This keeps the runner
simple and lets each environment orchestrate in its own way.
```

- [ ] **Step 2: Update README.md Runner section**

```markdown
## Runner

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results
cwl-zen validate workflow.cwl
cwl-zen dag workflow.cwl
```

Single static binary. Auto-detects container runtime (Podman, Apptainer, Singularity, Docker). No JS engine. Fast startup.

**Container support:**
- Docker, Podman: OCI images, pulled automatically
- Singularity, Apptainer: SIF images, cached in `~/.cwl-zen/containers/`
- Auto-detection: uses the first available runtime
- Override: `--engine singularity`
```

- [ ] **Step 3: Commit**

```bash
git add docs/spec.md README.md
git commit -m "docs: update spec and README with multi-engine container support"
```

---

## Task 10: Polish — Clippy, Full Test Suite, End-to-End

**Files:**
- All source files

- [ ] **Step 1: Run clippy**

```bash
cd ~/repos/cwl-zen && cargo clippy -- -W warnings
```

Fix any warnings.

- [ ] **Step 2: Run full test suite**

```bash
cd ~/repos/cwl-zen && cargo test
```

All tests must pass.

- [ ] **Step 3: End-to-end test — echo without container**

```bash
cd ~/repos/cwl-zen && rm -rf /tmp/cwl-zen-phase2-test
cargo run -- run tests/fixtures/echo.cwl tests/fixtures/echo-input.yml --outdir /tmp/cwl-zen-phase2-test
cat /tmp/cwl-zen-phase2-test/output.txt
# Expected: "Hello, CWL Zen!"
ls /tmp/cwl-zen-phase2-test/ro-crate-metadata.json
# Expected: exists
```

- [ ] **Step 4: End-to-end test — workflow**

```bash
cd ~/repos/cwl-zen && rm -rf /tmp/cwl-zen-phase2-wf
cargo run -- run tests/fixtures/two-step.cwl tests/fixtures/two-step-input.yml --outdir /tmp/cwl-zen-phase2-wf
# Expected: "Workflow completed successfully"
```

- [ ] **Step 5: Verify --help shows new flags**

```bash
cargo run -- run --help
# Expected: --engine, --container-cache, --copy-inputs, --no-retry-copy visible
```

- [ ] **Step 6: Commit if any fixes needed**

```bash
git add -A
git commit -m "chore: clippy fixes, verify full test suite and end-to-end"
```

---

## Summary

| Task | Module | What it produces |
|------|--------|------------------|
| 1 | container.rs | Engine trait, 4 backends, SIF caching, auto-detect |
| 2 | staging.rs | Symlink/copy staging, error detection |
| 3 | execute.rs | Integrate engine + staging, auto-retry, bug fixes |
| 4 | command.rs + parse.rs | Fix separate, add NetworkAccess |
| 5 | main.rs | New CLI flags |
| 6 | provenance.rs + input.rs | Fix dateModified, file:// URIs |
| 7 | stage.rs | Remove old build_docker_mounts |
| 8 | tests/ | Update integration tests |
| 9 | docs/ | Spec + README updates |
| 10 | polish | Clippy, full suite, end-to-end |

---

## Fixes from Self-Review

These must be applied by the implementing agents inline:

**1. `is_on_path()` must be `pub` in container.rs** — Task 5 calls `cwl_zen::container::is_on_path()`. Ensure it's exported as public.

**2. Engine detection failure should NOT fall back to Docker** — When no engine is found and no `--engine` flag is given, the runner should still work for tools without `DockerRequirement` (direct execution). The fallback in Task 5 should create a `DirectEngine` that runs commands without a container, or simply set `engine = None` and skip container logic in execute.rs when no DockerRequirement is present.

**3. `InitialWorkDirRequirement` parsing** — The spec says to support minimal `InitialWorkDirRequirement`. Add to `parse.rs`:
```rust
pub fn writable_inputs(tool: &CommandLineTool) -> Vec<String> {
    // Find InitialWorkDirRequirement, parse listing entries with writable: true,
    // extract input names from $(inputs.X) parameter references
}
```
In Task 3, when staging inputs, check `writable_inputs()` and use Copy mode for those specific files, Symlink for the rest.

**4. Retry workdir naming** — Instead of `workdir.with_extension("copy")`, use `workdir.parent().join(format!("{}_copy", step_name))`.

**5. `shellQuote` on InputBinding** — Add a concrete test in Task 4:
```rust
#[test]
fn shell_quote_false_on_input() {
    // Create a tool where an input has shellQuote: false
    // Verify the value is not quoted in the command line
}
```
The implementing agent should read the current command.rs to see if shellQuote is already handled on InputBinding and add handling if missing.

**6. `network_access` must flow from command.rs to container.rs** — In Task 3, when building `ContainerExecRequest`, set `network: resolved_cmd.network_access`.
