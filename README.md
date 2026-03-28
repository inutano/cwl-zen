# CWL Zen

**A Makefile with containers and typed files.**

CWL Zen is a minimal, JS-free subset of [CWL v1.2](https://www.commonwl.org/v1.2/) and a fast runner for it. It keeps the workflow layer purely declarative — tools handle logic, CWL handles plumbing.

> Trust the tools. Wire the plumbing. Nothing more.

## Why remove JavaScript?

CWL v1.2 allows embedding JavaScript via `InlineJavascriptRequirement`. This is powerful but costly:

**Requires a JS engine** in every runner implementation (Node.js, SpiderMonkey, etc.) — this is why writing a new CWL runner is hard. A minimal CWL runner without JS support could be ~2,000 lines of code. With JS, you inherit an entire language runtime.

**Breaks static analysis** — you can't validate a workflow without executing JS. Is this expression correct? You won't know until runtime:

```yaml
# Typo in 'sample_id' — no error until the workflow runs
valueFrom: $(inputs.sampl_id + ".bam")
```

**Hides logic in YAML strings** — debugging JS embedded in YAML is painful. Error messages point to CWL line numbers, not JS line numbers. You can't set breakpoints or step through expressions:

```yaml
# When this fails, good luck finding the bug:
valueFrom: |
  ${
    var parts = inputs.filename.split(".");
    var ext = parts[parts.length - 1];
    if (ext == "gz") return inputs.filename.replace(".gz", "");
    else return inputs.filename;
  }
```

**Reduces portability** — JS expressions behave differently across runners. Real examples:

```yaml
# What does $(inputs.optional_file.path) return when optional_file is null?
# cwltool:  "null" (string)
# Toil:     null (actual null, may throw)
# Arvados:  undefined behavior
# Your code depends on which runner you test with.

# What about $(inputs.count + 1) when count is a string "42"?
# JS coercion: "42" + 1 = "421" (string concatenation!)
# You expected: 43

# Array handling:
# $(inputs.files.length) — works in cwltool's JS engine
# But is 'length' guaranteed across all CWL JS implementations?
```

**Slows execution** — launching a JS interpreter for each `$(...)` expression adds overhead. In a workflow with 10 steps and 5 expressions each, that's 50 JS interpreter invocations. For a pipeline processing 400K samples, this adds up.

CWL Zen's answer: **you don't need any of this.** Every JS expression in a CWL workflow can be replaced with either a CWL parameter reference or a shell command. The result is simpler, faster, and more portable.

"But JS makes CWL more flexible!" — Yes, and that flexibility is the problem. When logic lives in JS expressions scattered across YAML files, workflows become programs. CWL Zen says: **workflows are not programs. They are wiring diagrams.** Logic belongs in tools (shell commands inside containers), not in the orchestration layer.

The same examples above, in CWL Zen:

```yaml
# Filename: just use parameter reference interpolation
valueFrom: $(inputs.sample_id).bam

# Strip .gz extension: do it in the tool's shell command
valueFrom: |
  F=$(inputs.filename.path)
  tool --input "\${F%.gz}"

# Null handling: test in shell, works the same everywhere
valueFrom: |
  F="$(inputs.optional_file.path)"
  if [ "\$F" != "null" ] && [ -e "\$F" ]; then
    tool "\$F"
  fi

# Arithmetic: shell, not JS
valueFrom: |
  RESULT=\$(($(inputs.count) + 1))
```

No JS engine. No portability surprises. No hidden logic.

> *The river doesn't decide where to go. The riverbed does.*

## Getting Started

### Check if your workflow is CWL Zen compatible

```bash
# Quick check — if any of these return results, you have work to do:
grep -rl 'InlineJavascriptRequirement' cwl/
grep -rn '\${' cwl/
grep -rn 'when:' cwl/
grep -rn 'parseInt\|parseFloat\|Math\.' cwl/
```

### Convert an existing workflow

See the [Refactoring Guide](docs/refactoring-guide.md) — 8 common JS patterns with before/after examples, plus the critical `\$` escaping rule for mixing CWL references with shell commands.

### Write a new CWL Zen workflow

See the [Spec](docs/spec.md) for supported features, or use the [Quick Example](#quick-example) below as a starting point.

## What CWL Zen removes

| Removed | Why | Alternative |
|---------|-----|-------------|
| `InlineJavascriptRequirement` | No JS engine needed | Parameter references + shell |
| `ExpressionTool` | One execution model, not two | Shell inside `CommandLineTool` |
| `when` conditionals | Requires JS | Null-tolerant tools |
| JS in `valueFrom`/`outputEval` | Requires JS | String interpolation + shell |
| `record`, `enum`, `Any` types | Complexity | Multiple simple inputs, `string` |

## What CWL Zen keeps

| Feature | Purpose |
|---------|---------|
| `CommandLineTool` + `Workflow` | The two classes — that's it |
| `baseCommand`, `arguments`, `inputs`, `outputs` | Tool definition |
| `inputBinding` (prefix, position, shellQuote) | Command construction |
| `outputBinding` (glob) | Output collection |
| `stdout` | Capture tool output |
| `DockerRequirement` (dockerPull) | Container per step |
| `ShellCommandRequirement` | Pipes, redirects, shell logic |
| `ResourceRequirement` | CPU/RAM hints for scheduler |
| `NetworkAccess` | Tools that need network |
| `secondaryFiles` | Index files (.bai, .fai) |
| `scatter` + `dotproduct` | Parallel over arrays |
| `StepInputExpressionRequirement` | `valueFrom` with parameter references in workflow steps |
| `File`, `string`, `int`, `boolean` + optional/array variants | Type system |
| `$(inputs.X)`, `$(runtime.X)`, `$(self)` | Parameter references (no JS) |

## Quick Example

```yaml
# aligner.cwl — a CWL Zen tool
cwlVersion: v1.2
class: CommandLineTool
baseCommand: [bwa-mem2, mem]
hints:
  DockerRequirement:
    dockerPull: "quay.io/biocontainers/bwa-mem2:2.2.1--he70b90d_8"
requirements:
  ResourceRequirement:
    coresMin: 8

inputs:
  reference:
    type: File
    secondaryFiles: [.0123, .amb, .ann, .bwt.2bit.64, .pac]
    inputBinding: { position: 1 }
  reads:
    type: File
    inputBinding: { position: 2 }
  sample_id:
    type: string

arguments:
  - prefix: -t
    valueFrom: $(runtime.cores)
  - prefix: -R
    valueFrom: "@RG\\tID:$(inputs.sample_id)\\tSM:$(inputs.sample_id)"

stdout: $(inputs.sample_id).sam

outputs:
  aligned:
    type: File
    outputBinding:
      glob: "$(inputs.sample_id).sam"
```

```yaml
# pipeline.cwl — a CWL Zen workflow
cwlVersion: v1.2
class: Workflow
requirements:
  StepInputExpressionRequirement: {}

inputs:
  sample_id: string
  reads: File
  reference: File

steps:
  align:
    run: aligner.cwl
    in:
      reference: reference
      reads: reads
      sample_id: sample_id
    out: [aligned]
  sort:
    run: sorter.cwl
    in:
      input_file: align/aligned
      sample_id: sample_id
    out: [sorted_bam]

outputs:
  bam:
    type: File
    outputSource: sort/sorted_bam
```

No JavaScript. No magic. Just tools and wiring.

## CWL Zen vs Make vs CWL v1.2

```
CWL v1.2 (full spec — JS, ExpressionTool, complex types)
  └── CWL Zen (strict subset — no JS, declarative only)
        └── Make (dependencies + commands, no containers, no types)
```

CWL Zen adds two things over Make:
1. **Containers per step** — each tool in its own isolated, versioned environment
2. **Typed files with associations** — `.bam` + `.bai` travel together

If you don't need these, a Makefile is enough.

## Documentation

- [**Spec**](docs/spec.md) — Supported features, escaping rules, runner design
- [**Refactoring Guide**](docs/refactoring-guide.md) — 8 patterns to eliminate JS from existing CWL workflows

## Runner

The CWL Zen runner is a planned Rust implementation:

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results
cwl-zen validate workflow.cwl
cwl-zen lint workflow.cwl     # check CWL Zen compatibility
cwl-zen dag workflow.cwl      # print step dependency graph
```

Single static binary. Singularity-native. No JS engine. Fast startup.

**Status:** Design complete, implementation not yet started.

## Compatibility

Every CWL Zen document is valid CWL v1.2. Run it on cwltool, Toil, or any compliant runner today:

```bash
cwltool workflow.cwl input.yml           # works now
cwl-zen run workflow.cwl input.yml       # works when runner ships
```

## Reference Implementation

[ChIP-Atlas Pipeline v2](https://github.com/inutano/chip-atlas-pipeline-v2) — 15 tools, 5 workflows, 400K+ genomics samples, zero JavaScript.

## License

MIT
