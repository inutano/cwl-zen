# CWL Zen

**A Makefile with containers and typed files.**

CWL Zen is a minimal, JS-free subset of [CWL v1.2](https://www.commonwl.org/v1.2/) and a fast runner for it. It keeps the workflow layer purely declarative — tools handle logic, CWL handles plumbing.

## Philosophy

> Trust the tools. Wire the plumbing. Nothing more.

- Tools are responsible for their own behavior
- The workflow layer only declares **what connects to what**
- All logic (conditionals, arithmetic, string manipulation) belongs in **shell commands inside tool containers**
- If a tool can't handle something, fix the tool, not the workflow
- The simplest CWL document that works is the best CWL document

## What CWL Zen removes from CWL v1.2

| Removed | Why | Alternative |
|---------|-----|-------------|
| `InlineJavascriptRequirement` | No JS engine needed | Parameter references + shell |
| `ExpressionTool` | One execution model, not two | Shell inside `CommandLineTool` |
| `when` conditionals | Requires JS | Null-tolerant tools |
| JS in `valueFrom`/`outputEval` | Requires JS | String interpolation + shell |

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
| `secondaryFiles` | Index files (.bai, .fai) |
| `scatter` + `dotproduct` | Parallel over arrays |
| `File`, `string`, `int`, `boolean` + optional/array variants | Type system |
| `$(inputs.X)`, `$(runtime.X)` | Parameter references (no JS) |

## CWL Zen vs Make vs CWL v1.2

```
CWL v1.2 (full spec — JS, ExpressionTool, complex types)
  └── CWL Zen (strict subset — no JS, declarative only)
        └── Make (dependencies + commands, no containers, no types)
```

CWL Zen adds two things over Make:
1. **Containers per step** — each tool runs in its own isolated, versioned environment
2. **Typed files with associations** — `.bam` + `.bai` travel together

## Quick Example

```yaml
# tool: aligner.cwl
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
# workflow: pipeline.cwl
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

## Documentation

- [**Spec**](docs/spec.md) — Complete CWL Zen subset specification, supported features, runner design
- [**Refactoring Guide**](docs/refactoring-guide.md) — How to eliminate JavaScript from existing CWL workflows (8 patterns with before/after examples)

## Runner

The CWL Zen runner (`cwl-zen`) is a planned Rust implementation:

```bash
cwl-zen run workflow.cwl input.yml --outdir ./results
cwl-zen validate workflow.cwl
cwl-zen lint workflow.cwl     # check CWL Zen compatibility
cwl-zen dag workflow.cwl      # print step dependency graph
```

Single static binary. Singularity-native. No JS engine. Fast startup.

**Status:** Design complete, implementation not yet started.

## Reference Implementation

The [ChIP-Atlas Pipeline v2](https://github.com/inutano/chip-atlas-pipeline-v2) is the first real-world pipeline built with CWL Zen — 15 tools, 5 workflows, processing 400K+ genomics samples, zero JavaScript.

## Relationship to CWL v1.2

Every CWL Zen document is valid CWL v1.2. You can run CWL Zen workflows on cwltool, Toil, or any compliant CWL runner today. The CWL Zen runner is an optional, minimal alternative.

```bash
# Works with cwltool
cwltool workflow.cwl input.yml

# Works with cwl-zen (when available)
cwl-zen run workflow.cwl input.yml
```

## License

MIT
