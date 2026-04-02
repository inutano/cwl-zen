# CWL Conformance Test Failure Analysis

**Date:** 2026-04-03
**Current score:** 31/244 passing (excluding inline_javascript, expression_tool)
**Target:** Fix high-impact issues to reach ~100+/244

## Research Summary

Two parallel research agents analyzed all 213 failing tests. Failures cluster into ~18 root cause categories. Many tests have overlapping failures — fixing the top 5 categories should unlock the majority.

---

## Unified Root Cause Categories (merged from both analyses)

### Tier 1: Highest Impact (fix these first, ~80+ tests unlocked)

**1. SHA1 checksum missing on File outputs**
- Impact: ALL tests with File outputs (50+ tests)
- Every conformance test checks `checksum: "sha1$..."`. cwl-zen doesn't compute or emit it.
- Fix: Add `checksum: Option<String>` to `FileValue`. Compute SHA1 in `from_path()`. Emit in `value_to_json()`. Add `sha1` crate dependency.
- Effort: Small
- Files: model.rs, param.rs, Cargo.toml

**2. Shorthand type parsing (`file1: File` map-form)**
- Impact: ~20 tests
- When map-form uses bare type strings like `file1: File`, `a: int`, `test: boolean`, the deserializer fails with "expected struct ToolInput".
- Fix: In `deserialize_map_or_list`, when value is a bare string, construct default struct with just `cwl_type: CwlType::Single(s)`.
- Effort: Medium
- Files: model.rs

**3. `cwl.output.json` support**
- Impact: ~10 tests
- Many conformance tests (esp. `args.py` pattern) write `cwl.output.json` which the runner should read for output values.
- Fix: After execution, check for `workdir/cwl.output.json`, parse as JSON, use as outputs.
- Effort: Small
- Files: execute.rs or stage.rs

**4. `stdin` redirect**
- Impact: ~4 tests
- `stdin` field is parsed but never wired into execution.
- Fix: Add `stdin_file` to ResolvedCommand, pipe it to command/container.
- Effort: Medium
- Files: command.rs, execute.rs, container.rs

**5. `loadContents` + `outputEval` on OutputBinding**
- Impact: ~12 tests
- OutputBinding only has `glob`. Missing `loadContents: bool` and `outputEval: String`.
- Fix: Add fields to model, read file contents when loadContents=true, evaluate outputEval with parameter refs.
- Effort: Medium
- Files: model.rs, stage.rs

### Tier 2: Medium Impact (~30 tests)

**6. `stderr` field + `type: stderr` handling**
- Impact: 3 tests
- Model missing `stderr: Option<String>`. `collect_outputs` doesn't handle `type: stderr`.
- Fix: Mirror the stdout implementation for stderr.
- Files: model.rs, command.rs, execute.rs, stage.rs

**7. Auto-generated stdout/stderr filenames**
- Impact: 2-3 tests
- When `type: stdout` but no explicit `stdout:` field, runner should auto-generate filename.
- Fix: Generate UUID-based filename when stdout/stderr field is None but output type requires it.
- Files: execute.rs, stage.rs

**8. Arg vector instead of string join**
- Impact: ~5 tests directly, improves correctness everywhere
- Commands joined into string then re-split by whitespace breaks multi-word args.
- Fix: Refactor ResolvedCommand to `args: Vec<String>` instead of `command_line: String`.
- Files: command.rs, container.rs, execute.rs

**9. Shell quoting (shellQuote: true/false)**
- Impact: 3+ tests
- When ShellCommandRequirement active, shellQuote=true args should be single-quoted.
- Fix: In command builder, apply shell escaping.
- Files: command.rs

**10. `itemSeparator` + array input command building**
- Impact: 2 tests
- `itemSeparator: ","` on array inputs not implemented. Per-item prefix not handled.
- Fix: Join array elements with itemSeparator, apply nested inputBinding.
- Files: command.rs

**11. File literals (`contents` field)**
- Impact: 2+ tests
- Input files with `contents: "..."` instead of `path` not supported.
- Fix: In input.rs, check for `contents`, create temp file.
- Files: input.rs

**12. Array source / MultipleInputFeatureRequirement**
- Impact: 3 tests
- `source: [a, b]` (array) not handled — only `source: "a"`.
- Fix: Change `source` to enum Single/Multiple. Resolve multiple sources into array.
- Files: model.rs, execute.rs

### Tier 3: Lower Impact / Large Effort

**13. Inline tool definitions (`run:` as mapping)**
- Impact: ~5 tests
- `run:` currently must be a string path. CWL allows inline tool definitions.
- Fix: Change `run` to enum Path/Inline.
- Files: model.rs, execute.rs

**14. Parameter reference expansion**
- Impact: ~5 tests
- `$(runtime)` as whole object, `$(inputs)` as whole object, nested property access `$(inputs.x.y.z)`.
- Fix: Extend param.rs resolver.
- Files: param.rs

**15. Record types**
- Impact: 3 tests
- Record inputs/outputs with per-field bindings.
- Fix: Add RecordType variant, ResolvedValue::Record.
- Files: model.rs, stage.rs, input.rs, param.rs

**16. Conditional `when` on workflow steps**
- Impact: 3 tests
- CWL Zen spec says "no when" but conformance tests need it.
- Decision: Skip or implement minimally (parameter reference only, no JS).
- Files: model.rs, execute.rs

**17. `dockerOutputDirectory`**
- Impact: 1 test
- Fix: Extract from requirements, adjust container output mount.
- Files: parse.rs, container.rs

**18. InitialWorkDirRequirement extensions**
- Impact: 1 test
- Only entryname/entry form supported. File staging by reference not handled.
- Files: parse.rs, execute.rs

### Out of Scope (by design)

- **InlineJavascriptRequirement** — CWL Zen is JS-free
- **ExpressionTool** — requires JS evaluation

---

## Recommended Implementation Order

### Phase A: Quick wins (~80+ tests, ~2 hours)
1. SHA1 checksum (unlocks all File output comparisons)
2. Shorthand type parsing (unlocks 20+ tests that currently fail to parse)
3. `cwl.output.json` support (unlocks args.py pattern tests)

### Phase B: Core features (~30 more tests, ~4 hours)
4. `stdin` redirect
5. `stderr` field + type + auto-filename
6. `loadContents` + `outputEval`
7. Arg vector refactor
8. Shell quoting

### Phase C: Edge cases (~20 more tests, ~4 hours)
9. `itemSeparator` + array input building
10. File literals
11. Array source / MultipleInputFeatureRequirement
12. Parameter reference expansion
13. Inline tool definitions

### Phase D: Optional (~10 more tests, large effort)
14. Record types
15. Conditional `when` (minimal)
16. `dockerOutputDirectory`
17. InitialWorkDirRequirement extensions

**Projected score after Phase A:** ~110/244 (45%)
**Projected score after Phase B:** ~140/244 (57%)
**Projected score after Phase C:** ~160/244 (66%)
