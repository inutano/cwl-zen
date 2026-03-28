# CWL Zen

## What is CWL Zen?

CWL Zen is three things:

1. **A philosophy** — trust the tools, keep the workflow layer minimal
2. **A CWL subset** — a strict subset of CWL v1.2 that is purely declarative
3. **A runner** — a fast, minimal implementation that executes this subset

## Philosophy

**"Trust the tools. Wire the plumbing. Nothing more."**

- Tools are responsible for their own behavior — reading inputs, handling edge cases, producing outputs
- The workflow layer only declares **what connects to what**
- All logic (conditionals, arithmetic, string manipulation) belongs in **shell commands inside tool containers**
- If a tool can't handle something, fix the tool (or its wrapper script), not the workflow
- The simplest CWL document that works is the best CWL document

### What CWL Zen is NOT

- Not a general-purpose programming environment embedded in YAML
- Not a place to put business logic, validation, or data transformation
- Not a framework that tries to anticipate every edge case

## CWL Zen Subset

### Two classes, that's it

| Class | Purpose |
|-------|---------|
| `CommandLineTool` | Defines a tool: command, inputs, outputs |
| `Workflow` | Wires tools together: step A's output → step B's input |

### CommandLineTool

```yaml
class: CommandLineTool
baseCommand: [tool, subcommand]
requirements:
  DockerRequirement:
    dockerPull: "quay.io/biocontainers/tool:version"
  ShellCommandRequirement: {}       # if you need pipes/redirects
  ResourceRequirement:              # optional: hints for scheduler
    coresMin: 8
    ramMin: 16384
  NetworkAccess:                    # optional: if tool needs network
    networkAccess: true

inputs:
  input_file:
    type: File
    inputBinding:
      prefix: --input
      position: 1
  sample_id:
    type: string

arguments:
  - prefix: --output
    valueFrom: $(inputs.sample_id).result.txt
  - prefix: --threads
    valueFrom: $(runtime.cores)

stdout: $(inputs.sample_id).log

outputs:
  result:
    type: File
    outputBinding:
      glob: "*.result.txt"
```

**Supported features:**

| Feature | Notes |
|---------|-------|
| `baseCommand` | Command to run |
| `arguments` | Static args and parameter references |
| `inputs` with `inputBinding` | `prefix`, `position`, `shellQuote` |
| `outputs` with `outputBinding` | `glob` for file collection |
| `stdout` | Capture stdout to file |
| `DockerRequirement` | `dockerPull` only — executed via Singularity |
| `ShellCommandRequirement` | For pipes, redirects, shell logic |
| `ResourceRequirement` | `coresMin`, `ramMin` — passed to scheduler |
| `NetworkAccess` | For tools that need network |
| `secondaryFiles` | Index files (.bai, .fai, etc.) |

**Not supported (by design):**

| Feature | Why not | Alternative |
|---------|---------|-------------|
| `InlineJavascriptRequirement` | No JS engine | Parameter references + shell |
| `InitialWorkDirRequirement` | Complex staging | Shell commands |
| `EnvVarRequirement` | Shell can do this | `export VAR=val` in command |
| `stdin` | Rarely needed | Shell redirect in command |
| `stderr` | Rarely needed | Shell redirect in command |
| `successCodes` | Trust the tool | Tool should exit properly |
| `loadContents` / `outputEval` | Needs JS | Read files in shell |

### Workflow

```yaml
class: Workflow
inputs:
  sample_id: string
  fastq: File
  reference: File

steps:
  align:
    run: tools/aligner.cwl
    in:
      reads: fastq
      ref: reference
      name: sample_id
    out: [bam]

  peaks:
    run: tools/peak-caller.cwl
    scatter: bam            # optional: parallel over arrays
    scatterMethod: dotproduct
    in:
      bam: align/bam
      name: sample_id
    out: [peaks]

outputs:
  result:
    type: File
    outputSource: peaks/peaks
```

**Supported features:**

| Feature | Notes |
|---------|-------|
| `steps` with `in`/`out` | Core wiring |
| `outputSource` | Connect step outputs to workflow outputs |
| `scatter` + `dotproduct` | Parallel execution over arrays |

**Not supported (by design):**

| Feature | Why not | Alternative |
|---------|---------|-------------|
| `when` | Needs JS | Handle null gracefully in tool |
| `SubworkflowFeatureRequirement` | Complexity | Flatten to one level |
| `MultipleInputFeatureRequirement` | `merge_flattened`, `pickValue` are complex | Redesign wiring |
| `StepInputExpressionRequirement` | Needs JS | Not needed |
| `scatterMethod: crossproduct` | Rarely needed | Bash loop |

### Types

| Type | Supported |
|------|-----------|
| `File` | Yes, with `secondaryFiles` |
| `File?` | Yes (optional) |
| `File[]` | Yes (arrays) |
| `string`, `int`, `float`, `boolean`, `long`, `double` | Yes |
| Optional variants (`string?`, etc.) | Yes |
| Array variants (`string[]`, etc.) | Yes |
| `null` | Yes |
| `Directory` | Yes |
| `record` | No — use multiple simple inputs |
| `enum` | No — use `string` |
| `Any` | No |

### Parameter References

Simple string interpolation — no JS, no expressions:

```
$(inputs.sample_id)              → input value
$(inputs.bam.path)               → file path
$(inputs.bam.basename)           → filename
$(inputs.bam.nameroot)           → filename without extension
$(inputs.bam.nameext)            → extension
$(inputs.bam.size)               → file size
$(runtime.cores)                 → CPU cores
$(runtime.ram)                   → RAM in MB
$(runtime.outdir)                → output directory
$(runtime.tmpdir)                → temp directory
```

Rules:
- `$(inputs.x)` is replaced with the value
- `"prefix_$(inputs.x)_suffix"` concatenation works
- No arithmetic: `$(1+1)` is NOT supported — use shell `$((1+1))`
- No function calls: `$(parseInt(...))` is NOT supported — use shell
- No conditionals: `$(x ? y : z)` is NOT supported — use shell `if`

## Runner

### What it does

```
CWL document + input YAML → parse → build DAG → execute steps → collect outputs
```

That's it. One workflow, one input, one run.

### CLI

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results
cwl-zen validate workflow.cwl
cwl-zen dag workflow.cwl            # print step graph
cwl-zen lint workflow.cwl           # check CWL Zen compatibility
```

### Implementation

```
cwl-zen/
├── parse.rs       — YAML → typed structs, resolve parameter refs
├── dag.rs         — step dependency graph from in/out wiring
├── execute.rs     — run steps in order, invoke Singularity/Docker
├── stage.rs       — mount inputs, collect outputs (glob)
└── main.rs        — CLI
```

**Language: Rust** — single static binary, fast startup, deploy anywhere on HPC.

### What it does NOT do

| Not the runner's job | Who does it |
|---------------------|-------------|
| Batch processing | User / AI agent / bash loop |
| Job scheduling | SLURM/SGE (user submits `cwl-zen run` as a job) |
| Retry on failure | User / wrapper script |
| Monitoring | External tools |
| Provenance / RO-Crate | Separate util (`cwl-zen-prov`) |
| Image pulling | `singularity pull` (done beforehand) |

## Ecosystem

Separate tools, not part of the runner:

| Tool | Purpose |
|------|---------|
| `cwl-zen-lint` | Validate CWL Zen compatibility (no JS, no ExpressionTool) |
| `cwl-zen-prov` | Generate RO-Crate provenance from completed runs |
| `cwl-zen-dispatch` | Helper scripts for SLURM/SGE/PBS job submission |

## Why not just use Make?

CWL Zen and Make are philosophically similar — both declare dependencies and run commands. The comparison is instructive:

| Aspect | Makefile | CWL Zen |
|--------|---------|---------|
| **Dependency unit** | Files on disk | Typed inputs/outputs (File, string, int) |
| **Containerization** | None (runs on host) | Built-in (Singularity/Docker per step) |
| **Portability** | Machine-specific (paths, installed tools) | Portable (containers carry the tools) |
| **Reproducibility** | Fragile (depends on host environment) | Strong (container versions pinned) |
| **Type safety** | None (everything is a filename string) | File vs string vs int, optional types, arrays |
| **Secondary files** | Manual (you track .bai alongside .bam) | Declarative (`secondaryFiles` pattern) |
| **Parallelism** | `make -j N` (file-level) | `scatter` (data-level: same tool on N samples) |
| **Intermediate files** | Stay on disk (you add `clean:` targets) | Managed by runner (staging/collection) |
| **Ecosystem** | Universal, 50 years of tooling | CWL runners, WES API, RO-Crate provenance |

**Same pipeline in Make vs CWL Zen:**

```makefile
# Makefile — you manage tools, paths, and file associations yourself
%.bam: %.fastq reference.fa
	bwa-mem2 mem -t 8 reference.fa $< | samtools sort -o $@
	samtools index $@

%.peaks: %.bam
	macs3 callpeak -t $< -n $* -g hs --nomodel
```

```yaml
# CWL Zen — containers and typed files, same simplicity
steps:
  align:
    run: bwa-mem2.cwl          # tool + container defined once
    in:
      fastq: fastq             # typed: File with .fai secondaryFiles
      reference: reference
    out: [bam]                 # bam + .bai travel together

  peaks:
    run: macs3.cwl
    in:
      bam: align/bam
    out: [peaks]
```

CWL Zen adds just two things over Make:

1. **Containers per step** — each tool runs in its own isolated, versioned environment. No "works on my machine" problems. Make can't do this without wrapper scripts everywhere.

2. **Typed files with associations** — when you declare `type: File` with `secondaryFiles: [.bai]`, the runner ensures the index travels with the BAM. In Make, you manually track both and hope they stay in sync.

If you don't need containers or typed file associations, a Makefile is enough. But for bioinformatics pipelines where tool version reproducibility and file associations (.bam+.bai, .fa+.fai+.0123+...) are critical, CWL Zen adds just enough structure.

**CWL Zen is a Makefile with containers and typed files.**

## Relationship to CWL v1.2

CWL Zen documents are a **strict subset** of CWL v1.2. Any CWL Zen document runs on cwltool, Toil, or any compliant CWL runner. The reverse is not true.

```
CWL v1.2 (full spec)
  └── CWL Zen (strict subset, no JS, no ExpressionTool)
```

`cwl-zen-lint` checks compatibility:
```bash
$ cwl-zen-lint workflow.cwl
PASS: No InlineJavascriptRequirement
PASS: No ExpressionTool
PASS: No 'when' conditionals
PASS: All valueFrom use parameter references only
PASS: CWL Zen compatible
```
