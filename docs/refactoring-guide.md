# CWL Zen Refactoring Guide: Eliminating JavaScript from CWL Workflows

A practical guide for converting CWL v1.2 workflows that use `InlineJavascriptRequirement` into CWL Zen — a strict, JS-free subset of CWL.

## Why

- No JavaScript engine needed in the runner (simpler, faster, more portable)
- Workflows become purely declarative — easier to validate, audit, and reason about
- Shell commands are explicit and debuggable — no hidden JS evaluation
- CWL Zen documents are still valid CWL v1.2 — they run on cwltool, Toil, or any compliant runner

## Before You Start

Check your current JS usage:

```bash
# Find files using InlineJavascriptRequirement
grep -rl 'InlineJavascriptRequirement' cwl/

# Find JS blocks (${...})
grep -rn '\${' cwl/

# Find 'when' clauses (require JS)
grep -rn 'when:' cwl/

# Find JS functions
grep -rn 'parseInt\|parseFloat\|Math\.' cwl/
```

## What Works WITHOUT InlineJavascriptRequirement

CWL v1.2 parameter references work without JS for **simple property access**:

```yaml
# All of these work WITHOUT InlineJavascriptRequirement:
$(inputs.sample_id)              # input value
$(inputs.bam.path)               # File path
$(inputs.bam.basename)           # filename
$(inputs.bam.nameroot)           # filename without extension
$(inputs.bam.nameext)            # extension
$(inputs.bam.size)               # file size
$(runtime.cores)                 # CPU cores
$(runtime.ram)                   # RAM in MB

# String interpolation (embedding references in strings):
"prefix_$(inputs.sample_id)_suffix"     # works!
"$(inputs.sample_id).sorted.bam"        # works!
```

## What REQUIRES InlineJavascriptRequirement (and how to replace it)

### 1. String Concatenation with `+` Operator

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
arguments:
  - prefix: -R
    valueFrom: $("@RG\\tID:" + inputs.sample_id + "\\tSM:" + inputs.sample_id)
```

**After (parameter reference interpolation):**
```yaml
arguments:
  - prefix: -R
    valueFrom: "@RG\\tID:$(inputs.sample_id)\\tSM:$(inputs.sample_id)"
```

**Rule:** Replace `$("prefix" + inputs.x + "suffix")` with `"prefix$(inputs.x)suffix"`.

### 2. Arithmetic Expressions

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
arguments:
  - shellQuote: false
    valueFrom: |
      bedtools genomecov -scale $(1000000 / inputs.mapped_read_count) ...
```

**After (shell arithmetic):**
```yaml
requirements:
  ShellCommandRequirement: {}
arguments:
  - shellQuote: false
    valueFrom: |
      SCALE=\$(awk -v n=\$(cat $(inputs.count_file.path)) 'BEGIN {printf "%.10f", 1000000/n}')
      bedtools genomecov -scale "\$SCALE" ...
```

**Rule:** Move arithmetic into shell (`awk`, `bc`, or shell arithmetic `$((...))`) inside the command. If the value comes from a previous step, pass it as a `File` containing the number, not a parsed `long`.

**Important:** Escape shell `$` as `\$` in CWL `valueFrom` strings. CWL interprets `$(...)` as a parameter reference. Use `\$(...)` for shell command substitution.

### 3. Conditional `${if (...)}` Blocks

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
arguments:
  - valueFrom: |
      ${
        if (inputs.fastq_rev) {
          return "--in-fq " + inputs.fastq_fwd.path + " " + inputs.fastq_rev.path;
        } else {
          return "--in-se-fq " + inputs.fastq_fwd.path;
        }
      }
    shellQuote: false
```

**After (shell if/else):**
```yaml
requirements:
  ShellCommandRequirement: {}
arguments:
  - shellQuote: false
    valueFrom: |
      REV="$(inputs.fastq_rev.path)"
      if [ "\$REV" != "null" ] && [ "\$REV" != "" ] && [ -e "\$REV" ]; then
        tool --in-fq $(inputs.fastq_fwd.path) "\$REV"
      else
        tool --in-se-fq $(inputs.fastq_fwd.path)
      fi
```

**Rule:** Move conditionals into shell `if/else`. For optional `File?` inputs, CWL resolves `$(inputs.optional_file.path)` to the string `"null"` when the input is not provided. Test for this in shell.

**Escaping pattern:**
- `$(inputs.X)` → CWL resolves this (no escape)
- `\$VAR` → shell variable reference (escaped from CWL)
- `"\$VAR"` → quoted shell variable (escaped from CWL)

### 4. Conditional `--out2` (Optional Arguments)

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
arguments:
  - prefix: --out2
    valueFrom: |
      ${
        if (inputs.fastq_rev) {
          return inputs.sample_id + "_trimmed_R2.fastq.gz";
        }
        return null;
      }
```

**After (unconditional argument):**
```yaml
arguments:
  - prefix: --out2
    valueFrom: $(inputs.sample_id)_trimmed_R2.fastq.gz
```

**Rule:** If the tool gracefully ignores the argument when not applicable (e.g., fastp ignores `--out2` when no `--in2` is given), just pass it unconditionally. Make the output `File?` so it returns null when no file is produced. Test this behavior with the specific tool.

### 5. `parseInt()` / `outputEval` with JS Functions

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
outputs:
  count:
    type: long
    outputBinding:
      glob: count.txt
      loadContents: true
      outputEval: $(parseInt(self[0].contents.trim()))
```

**After (output as File, consume in shell):**
```yaml
# Tool outputs a File instead of a parsed value
outputs:
  count_file:
    type: File
    outputBinding:
      glob: count.txt

# The consuming tool reads the file in its shell command
arguments:
  - shellQuote: false
    valueFrom: |
      VALUE=\$(cat $(inputs.count_file.path))
      tool --count "\$VALUE" ...
```

**Rule:** Don't parse values in CWL — output them as `File` and let the consuming tool read the file in its shell command. This eliminates the need for `loadContents`, `outputEval`, and JS parsing functions.

### 6. Conditional Workflow Steps (`when` clause)

**Before (JS):**
```yaml
# In workflow
steps:
  convert:
    run: converter.cwl
    when: $(inputs.input_file != null)
    in:
      input_file: previous_step/output
    out: [result]
```

**After (null-tolerant tool):**
```yaml
# In workflow — no 'when' clause
steps:
  convert:
    run: converter.cwl
    in:
      input_file: previous_step/output
    out: [result]

# In converter.cwl — handle null gracefully in shell
inputs:
  input_file:
    type: File?
arguments:
  - shellQuote: false
    valueFrom: |
      INPUT="$(inputs.input_file.path)"
      if [ "\$INPUT" != "null" ] && [ -e "\$INPUT" ]; then
        converter "\$INPUT" -o output.dat
      fi
outputs:
  result:
    type: File?    # null when input was null
    outputBinding:
      glob: "*.dat"
```

**Rule:** Remove `when` clauses from workflows. Instead, make the tool handle null/missing inputs gracefully in its shell command. The tool's output type should be `File?` so null propagates downstream.

### 7. Conditional Glob Patterns

**Before (JS):**
```yaml
requirements:
  InlineJavascriptRequirement: {}
outputs:
  sorted_bam:
    outputBinding:
      glob: |
        ${
          return inputs.by_name ? "*.namesorted.bam" : "*.sorted.bam";
        }
```

**After (glob array):**
```yaml
outputs:
  sorted_bam:
    outputBinding:
      glob:
        - "$(inputs.sample_id).sorted.bam"
        - "$(inputs.sample_id).namesorted.bam"
```

**Rule:** Use a glob array — CWL will return whichever file exists. Only one pattern needs to match.

### 8. `$(self).suffix` in Workflow Step Inputs

```yaml
# This works WITHOUT InlineJavascriptRequirement:
steps:
  peak_call:
    in:
      sample_id:
        source: sample_id
        valueFrom: $(self).05    # parameter reference + literal text
```

**Rule:** `$(self).suffix` is simple string interpolation, not JS. It requires `StepInputExpressionRequirement` but NOT `InlineJavascriptRequirement`. Keep `StepInputExpressionRequirement` in your workflow requirements.

## The Escaping Rule

This is the most common source of bugs during refactoring:

| In CWL valueFrom | Interpreted as | Example |
|-------------------|---------------|---------|
| `$(inputs.x)` | CWL parameter reference | `$(inputs.sample_id)` → `"SRX123"` |
| `\$(command)` | Shell command substitution | `\$(cat file.txt)` → contents of file |
| `\$VAR` | Shell variable | `\$SCALE` → value of SCALE |
| `"\$VAR"` | Quoted shell variable | `"\$SCALE"` → value of SCALE (safe) |

**When mixing CWL references and shell variables in the same command:**
```yaml
valueFrom: |
  COUNT=\$(cat $(inputs.count_file.path))
  #     ^^^ shell $()          ^^^ CWL $(inputs.X)
  echo "\$COUNT"
  #    ^^^ shell variable
```

## Checklist

After refactoring, verify:

```bash
# 1. No InlineJavascriptRequirement anywhere
grep -rl 'InlineJavascriptRequirement' cwl/ && echo "FAIL" || echo "PASS"

# 2. No JS blocks
grep -rn '\${' cwl/ && echo "FAIL" || echo "PASS"

# 3. No 'when' clauses
grep -rn 'when:' cwl/ && echo "FAIL" || echo "PASS"

# 4. No JS functions
grep -rn 'parseInt\|parseFloat\|Math\.' cwl/ && echo "FAIL" || echo "PASS"

# 5. All files validate
for f in cwl/tools/*.cwl cwl/workflows/*.cwl; do
  cwltool --validate "$f" 2>&1 | tail -1
done

# 6. End-to-end test produces identical results
diff <(wc -l original_output/*.narrowPeak) <(wc -l zen_output/*.narrowPeak)
```

## Summary

| JS Pattern | CWL Zen Replacement |
|-----------|-------------------|
| `$("a" + inputs.x + "b")` | `"a$(inputs.x)b"` (string interpolation) |
| `$(inputs.x + ".ext")` | `$(inputs.x).ext` (interpolation) |
| `$(math expression)` | `\$(awk '...')` (shell arithmetic) |
| `${if (x) return a; else return b}` | Shell `if/else` in valueFrom |
| `$(parseInt(self[0].contents))` | Output as `File`, read in consuming tool's shell |
| `when: $(inputs.x != null)` | Remove `when`, handle null in tool's shell |
| Conditional glob | Glob array (list all possible patterns) |
