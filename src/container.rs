use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{bail, Context, Result};

use crate::model::{FileValue, ResolvedValue};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A request to execute a command inside a container.
pub struct ContainerExecRequest {
    pub image: String,
    pub command: String,
    pub use_shell: bool,
    pub workdir: PathBuf,
    pub mounts: Vec<Mount>,
    pub network: bool,
    pub cores: u32,
    pub ram: u64,
    pub stdout: File,
    pub stderr: File,
}

/// A bind-mount specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub readonly: bool,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over container engines (Docker, Podman, Singularity, Apptainer).
pub trait ContainerEngine {
    fn name(&self) -> &str;
    fn pull(&self, image: &str, cache_dir: &Path) -> Result<()>;
    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus>;
}

// ---------------------------------------------------------------------------
// OciEngine — Docker / Podman
// ---------------------------------------------------------------------------

/// Container engine for OCI-compatible runtimes (Docker, Podman).
pub struct OciEngine {
    binary: String,
}

impl OciEngine {
    pub fn docker() -> Self {
        OciEngine {
            binary: "docker".to_string(),
        }
    }

    pub fn podman() -> Self {
        OciEngine {
            binary: "podman".to_string(),
        }
    }
}

impl OciEngine {
    /// Format a single mount as an OCI `--mount` flag value.
    pub fn format_mount(m: &Mount) -> String {
        let mut s = format!(
            "--mount=type=bind,source={},target={}",
            m.source.display(),
            m.target.display()
        );
        if m.readonly {
            s.push_str(",readonly");
        }
        s
    }
}

impl ContainerEngine for OciEngine {
    fn name(&self) -> &str {
        &self.binary
    }

    fn pull(&self, image: &str, _cache_dir: &Path) -> Result<()> {
        let status = Command::new(&self.binary)
            .args(["pull", image])
            .status()
            .with_context(|| format!("{}: failed to pull image {}", self.binary, image))?;
        if !status.success() {
            bail!("{}: pull failed for {}", self.binary, image);
        }
        Ok(())
    }

    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(["run", "--rm", "--workdir=/work"]);

        if !req.network {
            cmd.arg("--network=none");
        }

        for m in &req.mounts {
            cmd.arg(Self::format_mount(m));
        }

        cmd.arg(&req.image);

        if req.use_shell {
            cmd.args(["sh", "-c", &req.command]);
        } else {
            // Split command on whitespace for non-shell execution
            for token in req.command.split_whitespace() {
                cmd.arg(token);
            }
        }

        cmd.stdout(Stdio::from(req.stdout));
        cmd.stderr(Stdio::from(req.stderr));

        let status = cmd
            .status()
            .with_context(|| format!("{}: failed to execute container", self.binary))?;
        Ok(status)
    }
}

// ---------------------------------------------------------------------------
// SifEngine — Singularity / Apptainer
// ---------------------------------------------------------------------------

/// Container engine for SIF-based runtimes (Singularity, Apptainer).
pub struct SifEngine {
    binary: String,
}

impl SifEngine {
    pub fn singularity() -> Self {
        SifEngine {
            binary: "singularity".to_string(),
        }
    }

    pub fn apptainer() -> Self {
        SifEngine {
            binary: "apptainer".to_string(),
        }
    }
}

impl SifEngine {
    /// Format a single mount as a SIF `--bind` flag value.
    pub fn format_mount(m: &Mount) -> String {
        let mut s = format!("--bind={}:{}", m.source.display(), m.target.display());
        if m.readonly {
            s.push_str(":ro");
        }
        s
    }
}

impl ContainerEngine for SifEngine {
    fn name(&self) -> &str {
        &self.binary
    }

    fn pull(&self, image: &str, cache_dir: &Path) -> Result<()> {
        let sif_path = sif_cache_path(image, cache_dir);
        if sif_path.exists() {
            return Ok(());
        }
        // Ensure parent directory exists
        if let Some(parent) = sif_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create cache dir {}", parent.display()))?;
        }
        let docker_uri = format!("docker://{}", image);
        let status = Command::new(&self.binary)
            .args(["pull", &sif_path.to_string_lossy(), &docker_uri])
            .status()
            .with_context(|| format!("{}: failed to pull image {}", self.binary, image))?;
        if !status.success() {
            bail!("{}: pull failed for {}", self.binary, image);
        }
        Ok(())
    }

    fn exec(&self, req: ContainerExecRequest) -> Result<ExitStatus> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(["exec", "--pwd", "/work"]);

        for m in &req.mounts {
            cmd.arg(Self::format_mount(m));
        }

        cmd.arg(&req.image);

        if req.use_shell {
            cmd.args(["sh", "-c", &req.command]);
        } else {
            for token in req.command.split_whitespace() {
                cmd.arg(token);
            }
        }

        cmd.stdout(Stdio::from(req.stdout));
        cmd.stderr(Stdio::from(req.stderr));

        let status = cmd
            .status()
            .with_context(|| format!("{}: failed to execute container", self.binary))?;
        Ok(status)
    }
}

// ---------------------------------------------------------------------------
// SIF cache path derivation
// ---------------------------------------------------------------------------

/// Convert a Docker image reference to a local SIF file path.
///
/// Examples:
/// - `quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8`
///   -> `{cache}/quay.io/biocontainers/bwa-mem2_2.2.1--he70b90d_8.sif`
/// - `ubuntu:22.04` -> `{cache}/docker.io/library/ubuntu_22.04.sif`
/// - `user/repo:tag` -> `{cache}/docker.io/user/repo_tag.sif`
pub fn sif_cache_path(image: &str, cache_dir: &Path) -> PathBuf {
    // Determine if there is an explicit registry (contains a dot before the first slash)
    let (registry, remainder) = split_registry(image);

    // Split remainder into path and tag
    let (path_part, tag) = if let Some(colon_pos) = remainder.rfind(':') {
        (&remainder[..colon_pos], &remainder[colon_pos + 1..])
    } else {
        (remainder, "latest")
    };

    // Build the filename: last path component + "_" + tag + ".sif"
    let filename = if let Some(slash_pos) = path_part.rfind('/') {
        let name = &path_part[slash_pos + 1..];
        let dir_part = &path_part[..slash_pos];
        // directory/name_tag.sif under registry
        let sif_name = format!("{}_{}.sif", name, tag);
        cache_dir
            .join(registry)
            .join(dir_part)
            .join(sif_name)
    } else {
        // No slash in path — for docker.io this means library/name
        let sif_name = format!("{}_{}.sif", path_part, tag);
        if registry == "docker.io" {
            cache_dir.join(registry).join("library").join(sif_name)
        } else {
            cache_dir.join(registry).join(sif_name)
        }
    };

    filename
}

/// Split an image reference into (registry, remainder).
/// If no explicit registry is present, defaults to "docker.io".
fn split_registry(image: &str) -> (&str, &str) {
    // A registry is present if the part before the first '/' contains a dot or colon
    if let Some(slash_pos) = image.find('/') {
        let candidate = &image[..slash_pos];
        if candidate.contains('.') || candidate.contains(':') {
            return (candidate, &image[slash_pos + 1..]);
        }
    }
    ("docker.io", image)
}

// ---------------------------------------------------------------------------
// Cache dir resolution
// ---------------------------------------------------------------------------

/// Resolve the container cache directory.
///
/// Priority: explicit flag > `$CWL_ZEN_CONTAINER_CACHE` env > `~/.cwl-zen/containers/`
pub fn resolve_container_cache(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(env_val) = std::env::var("CWL_ZEN_CONTAINER_CACHE") {
        if !env_val.is_empty() {
            return PathBuf::from(env_val);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".cwl-zen").join("containers")
}

// ---------------------------------------------------------------------------
// Auto-detection
// ---------------------------------------------------------------------------

/// Priority order for container engine detection.
pub fn engine_priority_order() -> Vec<&'static str> {
    vec!["podman", "apptainer", "singularity", "docker"]
}

/// Check whether a binary is available on `$PATH`.
pub fn is_on_path(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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
    bail!(
        "No container engine found. Install one of: {}",
        engine_priority_order().join(", ")
    )
}

/// Create a container engine by name.
pub fn engine_by_name(name: &str) -> Result<Box<dyn ContainerEngine>> {
    match name {
        "docker" => Ok(Box::new(OciEngine::docker())),
        "podman" => Ok(Box::new(OciEngine::podman())),
        "singularity" => Ok(Box::new(SifEngine::singularity())),
        "apptainer" => Ok(Box::new(SifEngine::apptainer())),
        other => bail!("Unknown container engine: {}", other),
    }
}

// ---------------------------------------------------------------------------
// Build mounts helper
// ---------------------------------------------------------------------------

/// Build mount specifications from resolved inputs.
///
/// - Mount `workdir` as `/work` (read-write)
/// - Mount each input file's parent directory read-only (deduplicated)
/// - Handle secondary files recursively
pub fn build_mounts(
    inputs: &HashMap<String, ResolvedValue>,
    workdir: &Path,
) -> Vec<Mount> {
    let mut mounts = Vec::new();

    // Mount workdir as /work (read-write)
    let workdir_abs = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    mounts.push(Mount {
        source: workdir_abs,
        target: PathBuf::from("/work"),
        readonly: false,
    });

    // Collect unique parent directories from all file inputs
    let mut mounted_dirs: HashSet<PathBuf> = HashSet::new();
    collect_file_dirs(inputs, &mut mounted_dirs);

    // Sort for deterministic output
    let mut dirs: Vec<PathBuf> = mounted_dirs.into_iter().collect();
    dirs.sort();

    for dir in dirs {
        mounts.push(Mount {
            source: dir.clone(),
            target: dir,
            readonly: true,
        });
    }

    mounts
}

/// Recursively collect parent directories of all File/Directory values.
fn collect_file_dirs(inputs: &HashMap<String, ResolvedValue>, dirs: &mut HashSet<PathBuf>) {
    for val in inputs.values() {
        collect_file_dirs_from_value(val, dirs);
    }
}

/// Recursively extract parent directories from a single resolved value.
fn collect_file_dirs_from_value(val: &ResolvedValue, dirs: &mut HashSet<PathBuf>) {
    match val {
        ResolvedValue::File(fv) | ResolvedValue::Directory(fv) => {
            add_file_value_dirs(fv, dirs);
        }
        ResolvedValue::Array(arr) => {
            for item in arr {
                collect_file_dirs_from_value(item, dirs);
            }
        }
        _ => {}
    }
}

/// Add parent directory of a FileValue and its secondary files.
fn add_file_value_dirs(fv: &FileValue, dirs: &mut HashSet<PathBuf>) {
    let path = Path::new(&fv.path);
    if let Some(parent) = path.parent() {
        let parent_buf = parent.to_path_buf();
        if !parent_buf.as_os_str().is_empty() {
            dirs.insert(parent_buf);
        }
    }
    for sf in &fv.secondary_files {
        add_file_value_dirs(sf, dirs);
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // 1. oci_mount_format — readonly mount
    #[test]
    fn oci_mount_format() {
        let m = Mount {
            source: PathBuf::from("/data"),
            target: PathBuf::from("/data"),
            readonly: true,
        };
        let formatted = OciEngine::format_mount(&m);
        assert_eq!(
            formatted,
            "--mount=type=bind,source=/data,target=/data,readonly"
        );
    }

    // 2. oci_mount_readwrite — rw mount, no ",readonly" suffix
    #[test]
    fn oci_mount_readwrite() {
        let m = Mount {
            source: PathBuf::from("/work"),
            target: PathBuf::from("/work"),
            readonly: false,
        };
        let formatted = OciEngine::format_mount(&m);
        assert_eq!(
            formatted,
            "--mount=type=bind,source=/work,target=/work"
        );
        assert!(!formatted.contains("readonly"));
    }

    // 3. sif_mount_format — readonly
    #[test]
    fn sif_mount_format() {
        let m = Mount {
            source: PathBuf::from("/data"),
            target: PathBuf::from("/data"),
            readonly: true,
        };
        let formatted = SifEngine::format_mount(&m);
        assert_eq!(formatted, "--bind=/data:/data:ro");
    }

    // 4. sif_mount_readwrite — rw
    #[test]
    fn sif_mount_readwrite() {
        let m = Mount {
            source: PathBuf::from("/work"),
            target: PathBuf::from("/work"),
            readonly: false,
        };
        let formatted = SifEngine::format_mount(&m);
        assert_eq!(formatted, "--bind=/work:/work");
        assert!(!formatted.contains(":ro"));
    }

    // 5. sif_cache_path_derivation — full registry path with tag
    #[test]
    fn sif_cache_path_derivation() {
        let cache = PathBuf::from("/cache");
        let result = sif_cache_path(
            "quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8",
            &cache,
        );
        assert_eq!(
            result,
            PathBuf::from("/cache/quay.io/biocontainers/bwa-mem2_2.2.1--he70b90d_8.sif")
        );
    }

    // 6. sif_cache_path_dockerhub — bare image with docker.io/library/ prefix
    #[test]
    fn sif_cache_path_dockerhub() {
        let cache = PathBuf::from("/cache");
        let result = sif_cache_path("ubuntu:22.04", &cache);
        assert_eq!(
            result,
            PathBuf::from("/cache/docker.io/library/ubuntu_22.04.sif")
        );
    }

    // 7. detect_priority_order — verify order
    #[test]
    fn detect_priority_order() {
        let order = engine_priority_order();
        assert_eq!(order, vec!["podman", "apptainer", "singularity", "docker"]);
    }

    // 8. resolve_cache_dir_default — no env/flag -> ~/.cwl-zen/containers
    #[test]
    fn resolve_cache_dir_default() {
        // Temporarily remove the env var if set
        let old = std::env::var("CWL_ZEN_CONTAINER_CACHE").ok();
        std::env::remove_var("CWL_ZEN_CONTAINER_CACHE");

        let result = resolve_container_cache(None);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let expected = PathBuf::from(home).join(".cwl-zen").join("containers");
        assert_eq!(result, expected);

        // Restore
        if let Some(val) = old {
            std::env::set_var("CWL_ZEN_CONTAINER_CACHE", val);
        }
    }

    // 9. resolve_cache_dir_env — $CWL_ZEN_CONTAINER_CACHE takes precedence
    #[test]
    fn resolve_cache_dir_env() {
        let old = std::env::var("CWL_ZEN_CONTAINER_CACHE").ok();
        std::env::set_var("CWL_ZEN_CONTAINER_CACHE", "/tmp/my-cache");

        let result = resolve_container_cache(None);
        assert_eq!(result, PathBuf::from("/tmp/my-cache"));

        // Restore
        match old {
            Some(val) => std::env::set_var("CWL_ZEN_CONTAINER_CACHE", val),
            None => std::env::remove_var("CWL_ZEN_CONTAINER_CACHE"),
        }
    }

    // 10. resolve_cache_dir_explicit — explicit path overrides env
    #[test]
    fn resolve_cache_dir_explicit() {
        let old = std::env::var("CWL_ZEN_CONTAINER_CACHE").ok();
        std::env::set_var("CWL_ZEN_CONTAINER_CACHE", "/tmp/env-cache");

        let explicit = PathBuf::from("/opt/containers");
        let result = resolve_container_cache(Some(&explicit));
        assert_eq!(result, PathBuf::from("/opt/containers"));

        // Restore
        match old {
            Some(val) => std::env::set_var("CWL_ZEN_CONTAINER_CACHE", val),
            None => std::env::remove_var("CWL_ZEN_CONTAINER_CACHE"),
        }
    }
}
