# CWL Zen Phase 2: Multi-Engine Container Support and Staging

**Date:** 2026-04-02
**Status:** Approved design
**Depends on:** Phase 1 runner (complete)

## Summary

Add support for Singularity, Apptainer, and Podman alongside Docker. Auto-detect the available container runtime, auto-pull and cache SIF images for Singularity/Apptainer, stage inputs via symlinks with automatic copy-retry on failure. Bundle bug fixes from Phase 1 known gaps.

## Philosophy Update

> **One workflow, one input, one run.** CWL Zen runs a single workflow instance. Batching across samples — whether 10 or 400,000 — is the job of the caller: an AI agent, a SLURM array job, a bash loop. This keeps the runner simple and lets each environment orchestrate in its own way.
>
> CWL Zen may support simple `scatter` within a workflow in the future, but treats it as syntactic convenience, not a scaling mechanism. Large-scale fan-out is always external.

This should be added to the README and spec.

## 1. Container Engine Abstraction

### Trait

```rust
pub trait ContainerEngine: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn pull(&self, image: &str, cache_dir: &Path) -> Result<()>;
    fn exec(&self, req: &ContainerExecRequest) -> Result<ExitStatus>;
}

pub struct ContainerExecRequest {
    pub image: String,
    pub command: String,
    pub use_shell: bool,
    pub workdir: PathBuf,
    pub mounts: Vec<Mount>,
    pub network: bool,       // true if NetworkAccess requirement present
    pub cores: u32,
    pub ram: u64,
}

pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub readonly: bool,
}
```

### Implementations

Four engines, two code paths:

| Engine | Binary | Exec command | Image source | Mounts |
|--------|--------|-------------|-------------|--------|
| Podman | `podman` | `podman run --rm` | Docker URI directly | `--mount=type=bind,source=X,target=Y,readonly` |
| Docker | `docker` | `docker run --rm` | Docker URI directly | `--mount=type=bind,source=X,target=Y,readonly` |
| Apptainer | `apptainer` | `apptainer exec` | Local SIF file | `--bind X:Y:ro` |
| Singularity | `singularity` | `singularity exec` | Local SIF file | `--bind X:Y:ro` |

Docker and Podman share the same code path (OCI runtime). Singularity and Apptainer share the same code path (SIF runtime). The only difference within each pair is the binary name.

### Auto-detection

```rust
pub fn detect_engine() -> Result<Box<dyn ContainerEngine>>
```

Check PATH in priority order: `podman` → `apptainer` → `singularity` → `docker`. Return the first one found. If none found, error with:

```
No container runtime found. Install one of: podman, apptainer, singularity, docker
```

### CLI override

```bash
cwl-zen run --engine singularity workflow.cwl input.yml
cwl-zen run --engine docker workflow.cwl input.yml
```

Valid values: `podman`, `apptainer`, `singularity`, `docker`. If the specified engine is not on PATH, error immediately.

### Command generation

**Docker/Podman:**
```bash
{engine} run --rm \
  --workdir=/work \
  --mount=type=bind,source={workdir},target=/work \
  --mount=type=bind,source={input_dir},target={input_dir},readonly \
  [--network=none]              # unless NetworkAccess
  {image_uri} \
  [sh -c "{command}"]           # if use_shell, else split command
```

**Singularity/Apptainer:**
```bash
{engine} exec \
  --pwd /work \
  --bind {workdir}:/work \
  --bind {input_dir}:{input_dir}:ro \
  [--net --network=none]        # unless NetworkAccess
  {sif_path} \
  [sh -c "{command}"]           # if use_shell, else split command
```

## 2. SIF Image Caching

### Cache directory

```
~/.cwl-zen/
  containers/          # SIF image cache (long-lived, shareable across users)
    quay.io/
      biocontainers/
        bwa-mem2_2.2.1--he70b90d_8.sif
        samtools_1.18--h50ea8bc_1.sif
  runs/                # reserved for future run state/resume features
```

### Cache location priority

1. `--container-cache` CLI flag
2. `$CWL_ZEN_CONTAINER_CACHE` environment variable
3. `~/.cwl-zen/containers/` default

### Cache key derivation

Docker URI `quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8` becomes:
- Directory: `{cache_root}/quay.io/biocontainers/`
- Filename: `bwa-mem2_2.2.1--he70b90d_8.sif` (colon → underscore)

### Pull behavior

1. Before step execution, check if SIF exists at cache path
2. If not: `{engine} pull {sif_path} docker://{image_uri}` (Singularity/Apptainer accept this syntax; `--force` only if SIF already exists but is stale)
3. Log: `Pulling image: {image_uri} -> {sif_path}`
4. If pull fails: error with `Failed to pull image: {uri}. Run manually: {engine} pull docker://{uri}`

### Docker/Podman

No caching needed — they manage images internally. `pull()` is a no-op (the `run` command pulls automatically).

## 3. Input Staging

### Default: symlink staging

Before each step execution:

1. Create writable workdir: `{outdir}/.steps/{step_name}/`
2. For each File input: create symlink `{workdir}/{basename}` → `{actual_path}`
3. For each secondary file: also symlink
4. Mount workdir read-write, mount input parent directories read-only
5. Set container working directory to workdir
6. Tool runs, writes outputs into writable workdir
7. Collect outputs from workdir via glob

### Copy staging triggers

Three ways to trigger copy instead of symlink:

**A. Auto-retry on failure (default behavior):**

When a step fails, check stderr for symlink/permission error patterns:
- "Read-only file system"
- "Too many levels of symbolic links"
- "Operation not permitted"
- "Permission denied" (when writing next to an input)

If any pattern matches:
1. Log: `Step '{name}' failed. Retrying with copy-staged inputs (symlinks may have caused the failure)...`
2. Re-create workdir, this time copying files instead of symlinking
3. Re-run the step
4. If succeeds: `Step '{name}' succeeded after copy-staging. Consider adding InitialWorkDirRequirement with writable: true.`
5. If fails again: report the real error (not a staging issue)

Disable with `--no-retry-copy`.

**B. Per-tool CWL declaration (minimal InitialWorkDirRequirement):**

```yaml
requirements:
  InitialWorkDirRequirement:
    listing:
      - entry: $(inputs.bam)
        writable: true
```

CWL Zen supports a minimal subset:
- `entry`: must be a parameter reference to an input (`$(inputs.X)`)
- `writable: true`: copy this file instead of symlink
- `writable: false` (or absent): symlink (default)

No JavaScript, no dynamic listing, no `entryname`. Anything more complex should be done in shell.

**C. Global override flag:**

```bash
cwl-zen run --copy-inputs workflow.cwl input.yml
```

Forces all inputs to be copied for all steps. Useful for debugging or when the filesystem doesn't support symlinks.

## 4. Bug Fixes (bundled)

These are targeted fixes from the Phase 1 known gaps, addressed as part of this phase since we're already modifying the affected modules.

### 4.1 `inputBinding.separate` (command.rs)

When `separate: false`, format prefix+value without space:
```rust
if binding.separate.unwrap_or(true) {
    format!("{} {}", prefix, val)
} else {
    format!("{}{}", prefix, val)
}
```

### 4.2 `NetworkAccess` handling (container.rs)

Pass `network: bool` to ContainerExecRequest based on whether the tool has `NetworkAccess` in its requirements. Engines add `--network=none` by default and omit it when `network: true`.

### 4.3 `dateModified` in provenance (provenance.rs)

Use actual file modification time:
```rust
let modified = std::fs::metadata(&file.path)?.modified()?;
let dt: DateTime<Utc> = modified.into();
entity["dateModified"] = json!(format_timestamp(&dt));
```

### 4.4 Defaults from workflow/tool definitions (execute.rs)

Before resolving step inputs, merge workflow-level defaults:
```rust
for (name, wf_input) in &workflow.inputs {
    if !inputs.contains_key(name) {
        if let Some(default) = &wf_input.default {
            inputs.insert(name.clone(), yaml_to_resolved(default));
        }
    }
}
```

### 4.5 `file://` URI stripping (input.rs)

```rust
let path_str = path_str.strip_prefix("file://").unwrap_or(path_str);
```

### 4.6 `resolve_step_inputs` context (execute.rs)

Pass workflow-level inputs to `resolve_param_refs` in valueFrom resolution instead of empty HashMap:
```rust
let s = param::resolve_param_refs(vf, wf_inputs, runtime, Some(&base));
```

### 4.7 `inputBinding.shellQuote` (command.rs)

Verify that `shellQuote: false` on inputBinding is respected — the value should not be shell-quoted when building the command. Currently `shellQuote` on `ArgumentEntry` is handled but `shellQuote` on `InputBinding` may not be.

## 5. Module Structure

### New file: `src/container.rs`

```rust
// Trait definition
pub trait ContainerEngine: Send + Sync { ... }
pub struct ContainerExecRequest { ... }
pub struct Mount { ... }

// Auto-detection
pub fn detect_engine() -> Result<Box<dyn ContainerEngine>>
pub fn engine_by_name(name: &str) -> Result<Box<dyn ContainerEngine>>

// Implementations
struct DockerEngine;          // docker run
struct PodmanEngine;          // podman run (reuses Docker logic)
struct SingularityEngine;     // singularity exec
struct ApptainerEngine;       // apptainer exec (reuses Singularity logic)

// SIF caching
pub fn resolve_container_cache() -> PathBuf
pub fn sif_cache_path(image: &str, cache_dir: &Path) -> PathBuf
```

### Modified files

| File | Changes |
|------|---------|
| `src/execute.rs` | Replace direct Docker command with `ContainerEngine::exec()`. Add symlink staging with auto-retry. Apply bug fixes (defaults, resolve_step_inputs context). |
| `src/command.rs` | Remove Docker-specific logic (moved to container.rs). Fix `separate`, verify `shellQuote`. Now produces just the command string, not Docker arguments. |
| `src/stage.rs` | Add `stage_inputs()` function for symlink/copy staging. Remove `build_docker_mounts()` (moved to container.rs as Mount structs). |
| `src/main.rs` | Add `--engine`, `--container-cache`, `--copy-inputs`, `--no-retry-copy` flags. |
| `src/provenance.rs` | Fix `dateModified`. |
| `src/input.rs` | Fix `file://` URI stripping. |
| `src/model.rs` | Add `InitialWorkDirRequirement` to parsed requirements (minimal subset). |
| `src/parse.rs` | Add `has_initial_workdir_requirement()` and `writable_inputs()` helpers. |
| `docs/spec.md` | Update philosophy section. Move `InitialWorkDirRequirement` from "Not Supported" to "Partially Supported (writable: true only)". |

### Dependencies

No new crate dependencies. Everything uses `std::process::Command` and `std::os::unix::fs::symlink`.

## 6. CLI Changes

```bash
# New flags
cwl-zen run workflow.cwl input.yml \
  --engine singularity \          # override auto-detected engine
  --container-cache /shared/sif \ # override SIF cache location
  --copy-inputs \                 # force copy staging for all steps
  --no-retry-copy \               # disable auto-retry with copy on failure
  --outdir ./results

# Existing flags (unchanged)
  --no-crate                      # suppress RO-Crate provenance
```

## 7. Testing Strategy

### Unit tests

- `container.rs`: test mount generation for each engine, SIF cache path derivation, detection priority
- `stage.rs`: test symlink staging, copy staging, InitialWorkDirRequirement parsing
- Bug fix tests: separate, defaults, file:// URI, dateModified

### Integration tests

- `test_echo_singularity`: run echo tool via Singularity (skip if not available)
- `test_echo_podman`: run echo tool via Podman (skip if not available)
- `test_copy_retry`: tool that fails with symlinks succeeds after auto-retry
- `test_sif_caching`: verify SIF is created on first run, reused on second

Mark engine-specific tests with `#[ignore]` by default, run with `cargo test -- --ignored` on systems that have the runtime.

## References

- [CWL v1.2 InitialWorkDirRequirement](https://www.commonwl.org/v1.2/CommandLineTool.html#InitialWorkDirRequirement)
- [Singularity User Guide](https://docs.sylabs.io/guides/latest/user-guide/)
- [Apptainer User Guide](https://apptainer.org/docs/user/latest/)
- [Podman CLI reference](https://docs.podman.io/en/latest/)
