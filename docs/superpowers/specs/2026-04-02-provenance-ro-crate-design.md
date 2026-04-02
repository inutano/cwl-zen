# CWL Zen Provenance: Workflow Run RO-Crate

**Date:** 2026-04-02
**Status:** Approved design
**Profile:** Provenance Run Crate 0.5 (extends Workflow Run Crate 0.5, Process Run Crate 0.5)

## Summary

Every `cwl-zen run` produces a `ro-crate-metadata.json` in the output directory, making the outdir a valid [Workflow Run RO-Crate](https://www.researchobject.org/workflow-run-crate/). This records full provenance: the workflow definition, per-step execution details, input/output data with checksums, container images, and timing. Provenance is always-on by default, suppressible with `--no-crate`.

## Why

CWL Zen's runner parses the full workflow definition and orchestrates every step. It has native access to all the information needed for the richest provenance profile (Provenance Run Crate, Profile C). Sapporo WES could only reach Profile B (Workflow Run Crate) because it treats the workflow engine as a black box. cwl-zen has no such limitation.

Recording provenance by default means every run is reproducible and auditable without extra effort. The RO-Crate is a single JSON file alongside the outputs — zero friction.

## Architecture

### Execution flow

```
parse.rs → dag.rs → execute.rs → crate.rs → done
                                    ↑
                            Post-execution pass.
                            Receives RunResult struct
                            with all execution metadata.
                            Writes ro-crate-metadata.json
                            to outdir.
```

The provenance module (`crate.rs`) runs after the DAG completes (success or failure). It receives a `RunResult` struct containing the parsed workflow, per-step execution records, resolved inputs, and output file paths. It serializes this as JSON-LD and writes `ro-crate-metadata.json` into the outdir.

### Output structure

The outdir **is** the RO-Crate:

```
results/
  ro-crate-metadata.json     # provenance metadata
  workflow.cwl                # copy of the workflow file
  tools/                      # copies of tool .cwl files
    aligner.cwl
    sorter.cwl
  output.sorted.bam           # workflow outputs
  output.sorted.bam.bai
  logs/
    align.stdout.log
    align.stderr.log
    sort.stdout.log
    sort.stderr.log
```

### CLI

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results            # crate generated (default)
cwl-zen run workflow.cwl input.yml --outdir ./results --no-crate # crate suppressed
```

## Profile Conformance

The crate conforms to all three WRROC profiles plus Workflow RO-Crate:

| Profile | URI | Version |
|---------|-----|---------|
| Process Run Crate | `https://w3id.org/ro/wfrun/process/0.5` | 0.5 |
| Workflow Run Crate | `https://w3id.org/ro/wfrun/workflow/0.5` | 0.5 |
| Provenance Run Crate | `https://w3id.org/ro/wfrun/provenance/0.5` | 0.5 |
| Workflow RO-Crate | `https://w3id.org/workflowhub/workflow-ro-crate/1.0` | 1.0 |

## Entity Model

### Boilerplate (always present)

- **Metadata descriptor** (`ro-crate-metadata.json`) — `conformsTo` RO-Crate 1.1 + Workflow RO-Crate 1.0
- **Root Dataset** (`./`) — `conformsTo` all four profiles, `hasPart` listing all data entities, `mainEntity` pointing to workflow, `mentions` pointing to CreateAction
- **Profile CreativeWork stubs** — one per conformsTo target (4 entities)
- **ComputerLanguage** — CWL v1.2 (`https://w3id.org/workflowhub/workflow-ro-crate#cwl`)
- **SoftwareApplication** — cwl-zen itself, with version and URL

### Workflow-level

- **ComputationalWorkflow** — the .cwl file, `@type: [File, SoftwareSourceCode, ComputationalWorkflow]`, with `input`/`output` pointing to FormalParameter entities, `programmingLanguage` pointing to CWL
- **FormalParameter** — one per workflow input and output, with `additionalType` mapped from CWL types and `name` matching the CWL parameter name
- **HowToStep** — one per workflow step, representing the step definition in the DAG, with `position` reflecting execution order

### Run-level (top-level CreateAction)

- **OrganizeAction** — represents cwl-zen orchestrating the run; `instrument` → cwl-zen SoftwareApplication, `object` → workflow CreateAction, `result` → step ControlActions
- **CreateAction** (workflow) — `instrument` → ComputationalWorkflow, `object` → input data entities, `result` → output data entities, `startTime`, `endTime`, `actionStatus`
- **Input data** — `File` entities for file inputs (with `exampleOfWork` → FormalParameter), `PropertyValue` entities for non-file inputs (string, int, etc.)
- **Output data** — `File` entities with `contentSize`, `sha256`, `dateModified`, `encodingFormat`, `exampleOfWork` → FormalParameter

### Per-step (Profile C additions)

- **CreateAction** per step — `instrument` → tool SoftwareApplication, `object` → step inputs, `result` → step outputs, `startTime`, `endTime`, `exitCode`, `actionStatus`, `containerImage`
- **SoftwareApplication** per tool — the .cwl tool file reference
- **ContainerImage** — `@type: ContainerImage`, records the Docker/Singularity image URI from `DockerRequirement`
- **ControlAction** per step — links the OrganizeAction to each step's CreateAction via the HowToStep; records the orchestration relationship
- **Intermediate File entities** — outputs of one step that feed into the next (not workflow-level outputs)

### Logs

- stdout/stderr per step as `File` entities linked via `subjectOf` on each step's CreateAction
- Stored in `logs/` subdirectory of the outdir

## CWL Type to Schema.org Mapping

Used for `FormalParameter.additionalType`:

| CWL Zen Type | Schema.org `additionalType` | Notes |
|---|---|---|
| `File` | `File` | |
| `Directory` | `Dataset` | |
| `string` | `Text` | |
| `int`, `long` | `Integer` | |
| `float`, `double` | `Float` | |
| `boolean` | `Boolean` | |
| `T?` (optional) | Same as T | Plus `valueRequired: false` |
| `T[]` (array) | Same as T | Plus `multipleValues: true` |

## File Metadata

Every `File` entity includes:

| Property | Source |
|----------|--------|
| `contentSize` | `fs::metadata().len()` |
| `dateModified` | `fs::metadata().modified()`, ISO 8601 with `+00:00` offset |
| `sha256` | Streaming 64KB block hash (SHA-256 chosen over SHA-512 for speed; both are acceptable per RO-Crate spec) |
| `encodingFormat` | Extension-based EDAM lookup, then Tataki override if available |

### Timestamp format

All timestamps use ISO 8601 with explicit UTC offset `+00:00`, not `Z` suffix. The `roc-validator` tool rejects `Z`.

## Tataki Integration

Optional, best-effort enrichment for output file format detection.

### Flow

1. After crate JSON is built in memory, collect all output file paths
2. Run `tataki -f json -q <files...>` via `std::process::Command`
3. If exit 0: parse JSON (`{"/path/to/file": {"id": "http://edamontology.org/format_XXXX", "label": "BAM"}}`), match paths back to entities, replace `encodingFormat` with EDAM URI
4. If `tataki` not found on PATH or returns non-zero: silently skip, extension-based encodingFormat remains

### Fallback EDAM mapping (extension-based)

Used when Tataki is not available:

| Extension | EDAM ID | Label |
|-----------|---------|-------|
| `.bam` | `format_2572` | BAM |
| `.sam` | `format_2573` | SAM |
| `.cram` | `format_3462` | CRAM |
| `.vcf`, `.vcf.gz` | `format_3016` | VCF |
| `.bcf` | `format_3020` | BCF |
| `.fastq`, `.fq`, `.fastq.gz`, `.fq.gz` | `format_1930` | FASTQ |
| `.fasta`, `.fa`, `.fasta.gz`, `.fa.gz` | `format_1929` | FASTA |
| `.bed` | `format_3003` | BED |
| `.gff3`, `.gff` | `format_1975` | GFF3 |
| `.gtf` | `format_2306` | GTF |
| `.bw`, `.bigwig` | `format_3006` | bigWig |
| `.bb`, `.bigbed` | `format_3004` | bigBed |

## Module Structure

### `crate.rs`

```rust
/// Generate Provenance Run Crate metadata for a workflow run.
pub fn generate_crate(run_result: &RunResult, outdir: &Path) -> Result<()>

// Internal functions:
fn build_graph(run_result: &RunResult, outdir: &Path) -> Vec<serde_json::Value>
fn add_boilerplate(graph: &mut Vec<Value>)
fn add_workflow(graph: &mut Vec<Value>, workflow: &ParsedWorkflow)
fn add_organize_action(graph: &mut Vec<Value>, run_result: &RunResult)
fn add_workflow_action(graph: &mut Vec<Value>, run_result: &RunResult)
fn add_step_actions(graph: &mut Vec<Value>, steps: &[StepResult])
fn add_data_entities(graph: &mut Vec<Value>, run_result: &RunResult, outdir: &Path)
fn enrich_with_tataki(graph: &mut Vec<Value>, outdir: &Path)

fn file_metadata(path: &Path) -> Result<FileInfo>
fn cwl_type_to_schema_org(cwl_type: &str) -> &str
fn extension_to_edam(ext: &str) -> Option<(&str, &str)>
fn copy_workflow_to_outdir(workflow: &ParsedWorkflow, outdir: &Path) -> Result<()>
```

### `RunResult` struct (from `execute.rs`)

```rust
pub struct RunResult {
    pub workflow: ParsedWorkflow,
    pub inputs: ResolvedInputs,
    pub outputs: Vec<OutputFile>,
    pub steps: Vec<StepResult>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub success: bool,
}

pub struct StepResult {
    pub step_name: String,
    pub tool_path: PathBuf,
    pub container_image: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub exit_code: i32,
    pub inputs: Vec<StepInput>,
    pub outputs: Vec<OutputFile>,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
}
```

### Dependencies

No new external Rust crate dependencies beyond what the runner already needs:

| Crate | Purpose |
|-------|---------|
| `serde_json` | JSON-LD serialization |
| `sha2` | SHA-256 checksums for file entities |
| `chrono` | Timestamp formatting (ISO 8601 with +00:00 offset) |
| `uuid` | Generate `@id` values for CreateAction entities |

Tataki is invoked as a subprocess (`std::process::Command`), not a crate dependency.

## Integration

```rust
// main.rs — run command handler
let run_result = execute::run(&dag, &inputs, &outdir)?;

if !args.no_crate {
    crate::generate_crate(&run_result, &outdir)?;
}
```

## Validation

The generated `ro-crate-metadata.json` should be validated against:

- [`roc-validator`](https://github.com/ResearchObject/roc-validator) — validates profile conformance
- [`ro-crate-py`](https://github.com/ResearchObject/ro-crate-py) — can load and inspect the crate programmatically
- Manual inspection against the Sapporo test fixture at `~/repos/sapporo-service/tests/ro-crate/ro-crate-metadata.json`

## References

- [Workflow Run RO-Crate spec](https://www.researchobject.org/workflow-run-crate/)
- [Provenance Run Crate profile](https://w3id.org/ro/wfrun/provenance/0.5)
- [Sapporo RO-Crate implementation](https://github.com/sapporo-wes/sapporo-service/blob/main/sapporo/ro_crate.py)
- [Tataki file format detector](https://github.com/sapporo-wes/tataki)
- [RO-Crate 1.1 spec](https://w3id.org/ro/crate/1.1)
