# CWL Zen Specification

CWL Zen is a strict subset of CWL v1.2. This document defines exactly what is supported and what is not.

## Classes

| Class | Supported |
|-------|-----------|
| `CommandLineTool` | Yes |
| `Workflow` | Yes |
| `ExpressionTool` | **No** — use shell inside `CommandLineTool` |

## CommandLineTool Features

### Supported

| Feature | Notes |
|---------|-------|
| `cwlVersion: v1.2` | Required |
| `baseCommand` | Command to run |
| `arguments` | Static args and parameter references |
| `inputs` with `inputBinding` | `prefix`, `position`, `separate`, `shellQuote` |
| `outputs` with `outputBinding` | `glob` (with parameter references) |
| `stdout` | Capture stdout to file |
| `secondaryFiles` | On inputs and outputs |
| `DockerRequirement` | `dockerPull` only |
| `ShellCommandRequirement` | For pipes, redirects, shell logic |
| `ResourceRequirement` | `coresMin`, `ramMin` |
| `NetworkAccess` | For tools that need network |

### Not Supported

| Feature | Why | Alternative |
|---------|-----|-------------|
| `InlineJavascriptRequirement` | No JS engine | Parameter references + shell |
| `InitialWorkDirRequirement` (minimal) | `writable: true` only | Copy-staging for specific inputs |
| `EnvVarRequirement` | Unnecessary | `export VAR=val` in shell command |
| `stdin` | Rarely needed | Shell redirect: `< file` |
| `stderr` | Rarely needed | Shell redirect: `2> file` |
| `successCodes` | Trust the tool | Tool should exit properly |
| `outputEval` with JS | Requires JS | Output as File, read in consuming tool's shell |

### `loadContents` and `outputEval`

`loadContents: true` itself is a CWL feature, not JavaScript. However, useful `outputEval` expressions (like `$(parseInt(self[0].contents))`) require JS.

**CWL Zen approach:** Don't parse values in CWL. Output raw data as a `File` and let the consuming tool read it in its shell command:

```yaml
# Producer: output count as a text file
outputs:
  count_file:
    type: File
    outputBinding:
      glob: count.txt

# Consumer: read the file in shell
arguments:
  - shellQuote: false
    valueFrom: |
      COUNT=\$(cat $(inputs.count_file.path))
      tool --count "\$COUNT"
```

## Workflow Features

### Supported

| Feature | Notes |
|---------|-------|
| `steps` with `in`/`out` | Core wiring |
| `outputSource` | Connect step outputs to workflow outputs |
| `scatter` | Single input or multiple |
| `scatterMethod: dotproduct` | Zip arrays element-wise |
| `ScatterFeatureRequirement` | Required for `scatter` |
| `StepInputExpressionRequirement` | For `valueFrom` with parameter references (not JS) |

### Not Supported

| Feature | Why | Alternative |
|---------|-----|-------------|
| `when` | Requires JS (`$(inputs.x != null)`) | Make tools null-tolerant |
| `SubworkflowFeatureRequirement` | Complexity | Flatten to one workflow level |
| `MultipleInputFeatureRequirement` | `merge_flattened`, `pickValue` are complex | Redesign step wiring |
| `scatterMethod: flat_crossproduct` | Rarely needed | Bash loop |
| `scatterMethod: nested_crossproduct` | Rarely needed | Bash loop |

### `StepInputExpressionRequirement` — Supported (without JS)

`StepInputExpressionRequirement` enables `valueFrom` on workflow step inputs. CWL Zen supports this for **parameter references only** — no JavaScript:

```yaml
# SUPPORTED — parameter reference + literal text:
requirements:
  StepInputExpressionRequirement: {}
steps:
  peak_call:
    in:
      sample_id:
        source: sample_id
        valueFrom: $(self).q05       # $(self) is a parameter reference
```

```yaml
# NOT SUPPORTED — JavaScript expression:
steps:
  peak_call:
    in:
      sample_id:
        source: sample_id
        valueFrom: $(self + "_q05")  # string concatenation is JS
```

The difference: `$(self).q05` is `$(self)` (parameter reference) followed by literal text `.q05`. This is string interpolation. `$(self + "_q05")` uses the `+` operator, which is JavaScript.

## Type System

### Supported

| Type | Notes |
|------|-------|
| `File` | With `secondaryFiles` |
| `Directory` | |
| `string` | |
| `int`, `long` | |
| `float`, `double` | |
| `boolean` | |
| `null` | |
| Optional: `File?`, `string?`, etc. | |
| Arrays: `File[]`, `string[]`, etc. | |

### Not Supported

| Type | Alternative |
|------|-------------|
| `record` | Use multiple simple inputs |
| `enum` | Use `string` |
| `Any` | Use specific type |

## Parameter References

CWL Zen uses CWL v1.2 parameter references — **not JavaScript**. These work without `InlineJavascriptRequirement`:

```
$(inputs.sample_id)              → input value
$(inputs.bam.path)               → file path
$(inputs.bam.basename)           → filename
$(inputs.bam.nameroot)           → filename without extension
$(inputs.bam.nameext)            → extension
$(inputs.bam.size)               → file size in bytes
$(runtime.cores)                 → allocated CPU cores
$(runtime.ram)                   → allocated RAM in MB
$(runtime.outdir)                → output directory
$(runtime.tmpdir)                → temporary directory
$(self)                          → current value (in valueFrom)
```

### String interpolation

Parameter references can be embedded in strings:

```yaml
# All valid — no JS needed:
stdout: $(inputs.sample_id).sam
valueFrom: "@RG\\tID:$(inputs.sample_id)\\tSM:$(inputs.sample_id)"
glob: "$(inputs.sample_id).sorted.bam"
```

### What parameter references CANNOT do

```yaml
# Arithmetic — use shell instead
$(1000000 / inputs.count)           # NO
\$(awk 'BEGIN {print 1000000/n}')   # YES (in shell)

# Function calls — use shell instead
$(parseInt(self[0].contents))       # NO
\$(cat file.txt)                    # YES (in shell)

# Conditionals — use shell instead
$(inputs.x ? "a" : "b")            # NO
if [ ... ]; then ...; fi            # YES (in shell)

# String concatenation with + operator
$(inputs.x + ".suffix")            # NO
$(inputs.x).suffix                  # YES (interpolation)
```

## The Escaping Rule

When writing shell commands in CWL `valueFrom`, you must distinguish between CWL parameter references and shell syntax:

| Syntax | Meaning | Example |
|--------|---------|---------|
| `$(inputs.x)` | CWL resolves this | `$(inputs.sample_id)` → `"SRX123"` |
| `\$(command)` | Shell command substitution | `\$(cat file.txt)` → file contents |
| `\$VAR` | Shell variable | `\$SCALE` → value of SCALE |
| `"\$VAR"` | Quoted shell variable | `"\$SCALE"` → value of SCALE (safe) |

**Example mixing both:**

```yaml
arguments:
  - shellQuote: false
    valueFrom: |
      COUNT=\$(cat $(inputs.count_file.path))
      #     ^^^                ^^^ CWL parameter reference
      #     shell command substitution
      SCALE=\$(awk -v n=\$COUNT 'BEGIN {printf "%.10f", 1000000/n}')
      #                 ^^^ shell variable
      tool -s "\$SCALE" -i $(inputs.bam.path)
      #       ^^^ shell var   ^^^ CWL ref
```

**Rule of thumb:** If it's a CWL input or runtime value, use `$(...)`. If it's a shell variable or command, use `\$(...)` or `\$VAR`.

## Null Handling

When a `File?` input is null, `$(inputs.optional_file.path)` resolves to the string `"null"` in most CWL runners. Use this in shell to test:

```yaml
inputs:
  optional_file:
    type: File?

arguments:
  - shellQuote: false
    valueFrom: |
      F="$(inputs.optional_file.path)"
      if [ "\$F" != "null" ] && [ -e "\$F" ]; then
        process "\$F"
      fi
```

This pattern replaces `when: $(inputs.optional_file != null)` in workflows — the tool handles null gracefully instead of the workflow skipping the step.

## Runner Design

### Execution model

```
CWL document + input YAML → parse → build DAG → execute steps → collect outputs
```

One workflow, one input, one run. Batching across samples — whether 10 or 400,000 — is the job of the caller: an AI agent, a SLURM array job, a bash loop. This keeps the runner simple and lets each environment orchestrate in its own way.

### CLI

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results
cwl-zen validate workflow.cwl
cwl-zen lint workflow.cwl           # check CWL Zen compatibility
cwl-zen dag workflow.cwl            # print step dependency graph
```

### Container engines

cwl-zen auto-detects the available container runtime in this priority order:
`podman` → `apptainer` → `singularity` → `docker`

Override with `--engine`:

```bash
cwl-zen run --engine singularity workflow.cwl input.yml
```

SIF images for Singularity/Apptainer are cached in `~/.cwl-zen/containers/`.
Override with `--container-cache` or `$CWL_ZEN_CONTAINER_CACHE`.

### Input staging

Input files are staged into the step working directory as symlinks by default. If a tool fails due to symlink issues, cwl-zen automatically retries with copied files.

Force copy staging: `--copy-inputs`
Disable auto-retry: `--no-retry-copy`

### Implementation

```
cwl-zen/
├── parse.rs       — YAML → typed structs, resolve parameter refs
├── dag.rs         — step dependency graph from in/out wiring
├── execute.rs     — run steps in order, invoke Singularity/Docker
├── stage.rs       — mount inputs, collect outputs (glob)
└── main.rs        — CLI
```

**Language:** Rust — single static binary, fast startup, deploy anywhere on HPC.

### Not the runner's job

| Concern | Who handles it |
|---------|---------------|
| Batch processing | User / AI agent / bash loop |
| Job scheduling | SLURM/SGE (submit `cwl-zen run` as a job) |
| Retry on failure | User / wrapper script |
| Monitoring | External tools |
| Provenance / RO-Crate | `cwl-zen-prov` (separate tool) |
| Image pulling | `singularity pull` (done beforehand) |

## Ecosystem

Separate tools, not part of the runner:

| Tool | Purpose |
|------|---------|
| `cwl-zen-lint` | Validate CWL Zen compatibility |
| `cwl-zen-prov` | Generate RO-Crate provenance |
| `cwl-zen-dispatch` | Helper scripts for SLURM/SGE/PBS |

## Compatibility

CWL Zen documents are a **strict subset** of CWL v1.2:

```
CWL v1.2 (full spec)
  └── CWL Zen (no JS, no ExpressionTool, no when)
```

- Any CWL Zen document runs on cwltool, Toil, or any CWL v1.2 runner
- The reverse is not true — CWL documents with JS are not CWL Zen compatible
- `cwl-zen-lint` checks compatibility
