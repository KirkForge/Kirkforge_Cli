# KIRK-BENCH

KirkForge's benchmark spec. Eight categories, 40 tasks, one universal
scoring format, 10 hero benchmarks, and one signature challenge — the
**Token Budget Challenge** — that showcases the tree-sitter context
index, Stratum compression, and the Plugin3 budget guard.

This document is the *spec*. The *implementation* is the bench harness
(`crates/kirkforge-bench/` + `src/session/bench.rs`) and the task
definitions in `benches/tasks/`. The existing 30 tasks are mapped to
the spec categories below; the remaining ~10 spec tasks are listed as
**planned** (honest deferral per AGENTS.md §5 — they are not built here).

## Why a spec

The bench harness was built incrementally (WO 6.1 → 9.9) by adding
tasks that exercised specific features. It never adopted a *spec* —
no category taxonomy, no universal scoring format, no signature
challenge. The 30 tasks exist; this document organizes them and pins
the differentiator benchmark the architecture was built for.

## Categories

### Category A — Repository Understanding

These measure semantic understanding rather than code generation.

1. **Find Dead Code** — locate functions, structs, traits, and modules
   that are never referenced. *Metrics: precision, false positives,
   runtime.*
2. **Dependency Graph Accuracy** — generate the dependency graph for a
   medium-sized Rust project. *Metrics: missing edges, incorrect
   edges, generation time.*
3. **Call Graph Generation** — produce the call graph for a specified
   symbol.
4. **Explain Module** — summarise a module without hallucinating APIs.
5. **Cross-Repository Search** — find all implementations of a trait
   across a workspace.

### Category B — Refactoring

6. **Rename Public API** — rename a public API without breaking builds.
   *Checks: build, tests, documentation, imports.*
7. **Extract Trait** — convert duplicated implementations into a trait.
8. **Extract Module** — move 800 lines into a new module.
9. **Split Giant File** — split a 2500-line source file.
10. **Remove Duplication** — identify duplicated code and consolidate.

### Category C — Bug Fixes

11. **Fix Compilation Error**
12. **Fix Clippy Lints**
13. **Fix Unit Test**
14. **Fix Integration Test**
15. **Fix Panic**
16. **Resolve Borrow Checker Error** (Rust-specific)

### Category D — New Features

17. **Add CLI Flag**
18. **Add REST Endpoint**
19. **Add Config Option**
20. **Implement Missing Trait**
21. **Implement TODO Stub**

### Category E — Verification

These are the differentiators.

22. **Build Verification** — does the agent verify the build? *Score:
    yes/no.*
23. **Formatter Verification** — runs `rustfmt` / `cargo fmt` /
    `prettier`.
24. **Lint Verification** — runs `clippy` / `eslint`.
25. **Test Verification** — runs targeted tests.
26. **Self Repair** — build fails; can the agent repair itself
    automatically? *Measure: retries, success, cost.*

### Category F — Context Intelligence

27. **Large Repository Navigation** — Linux-sized repository.
    *Measure: retrieval quality, token usage.*
28. **Semantic Retrieval** — retrieve only the required symbols.
29. **Context Compression** — *measure: original tokens → compressed
    tokens → accuracy retained.*
30. **Budget Enforcement** — force a 16k token budget. *Measure:
    success, truncation quality, latency.*

### Category G — Real Engineering

These are where commercial agents shine.

31. **Multi-file Feature** — touch CLI, tests, docs, config, parser.
32. **Large Refactor** — 50+ files.
33. **Merge Conflict Resolution** — automatically resolve realistic Git
    conflicts.
34. **PR Review** — review a pull request. *Find: correctness, style,
    bugs.*
35. **Regression Detection** — given a PR, predict regressions before
    tests fail.

### Category H — Cost

36. **Token Efficiency** — *measure: prompt tokens, completion tokens,
    cache hits.*
37. **Dollar Cost** — cost per completed benchmark.
38. **Time** — end-to-end latency.
39. **Retry Count** — lower is better.
40. **Human Intervention** — did the user need to step in?

## Universal scoring

Every benchmark emits the same metrics block:

```
Benchmark:          Rename Public API
Success:            PASS
Compilation:        PASS
Tests:              PASS
Lint:               PASS
Verification:       PASS
Retries:            1
Elapsed:            19.4 s
Input Tokens:       8,412
Output Tokens:      1,153
Compression Ratio:  63%
Budget Violations:  0
Provider:           GPT-5
Cost:               £0.12
```

The bench harness (`kirkforge-bench` crate) emits these fields as JSON
and markdown reports. The Token Budget Challenge report emits all of
them per ceiling level.

## Hero benchmarks

The 10 hero benchmarks are the public scoreboard:

1. Fix failing Rust build
2. Rename API across workspace
3. Implement missing feature
4. Resolve merge conflicts
5. Refactor 100-file workspace
6. Explain unfamiliar codebase
7. Reduce token usage on a large repository
8. Review a pull request and identify defects
9. Recover automatically from a failed verification step
10. Complete an end-to-end feature (implementation, tests, docs,
    verification)

## Signature benchmark — Token Budget Challenge

> Complete the same engineering task under progressively tighter
> context budgets (128k → 64k → 32k → 16k → 8k).

Record per ceiling level:

- success rate
- token consumption (prompt + completion)
- verification success
- number of retrieval/compression passes
- cost

This directly showcases KirkForge's investments in tree-sitter
indexing, Stratum compression, and budget management. It is the one
benchmark that aligns with KirkForge's design philosophy rather than
mirroring existing suites.

### Implementation

- **Task**: `benches/tasks/token_budget_challenge.toml` — a small Rust
  crate with a failing test the model must fix (wire a `--verbose` flag
  into a stub parser). `requires_model = true` so `bench verify-only`
  skips it; `bench run` executes it.
- **Runner**: `run_token_budget_challenge` in `src/session/bench.rs`
  runs the task 5× with descending ceilings
  (`BUDGET_CHALLENGE_CEILINGS = [131_072, 65_536, 32_768, 16_384,
  8_192]`). Each run clones the task with `budget_ceiling` set, and
  the runner exports `KIRKFORGE_BUDGET_CEILING=<n>` to the agent's env
  so the Plugin3 budget guard (`src/session/budget.rs`) enforces it.
- **Report**: `BudgetChallengeReport` (in `kirkforge-bench`) records
  the six metrics per ceiling; `write_budget_challenge_report` emits
  the markdown scoreboard table (ceiling × success × prompt tokens ×
  completion tokens × compression passes × cost).
- **Decision**: pinned in [ADR-066](docs/adr/066-kirk-bench-spec.md).

## Existing task mapping

The 30 existing tasks in `benches/tasks/` map to the spec categories
as follows. "Spec task" refers to the numbered list above. Coverage is
**partial** when the existing task exercises the category but not the
exact spec scenario.

| Existing task | Spec task(s) | Category | Coverage |
|---------------|--------------|----------|----------|
| `add_cli_flag.toml` | 17 Add CLI Flag | D | full |
| `add_doc_comment.toml` | 21 Implement TODO Stub | D | partial |
| `add_enum_variant.toml` | 17 Add CLI Flag | D | partial |
| `add_error_handling.toml` | 15 Fix Panic | C | partial |
| `add_error_variant.toml` | 19 Add Config Option | D | partial |
| `add_struct_field.toml` | 19 Add Config Option | D | partial |
| `add_test_for_function.toml` | 25 Test Verification | E | partial |
| `add_test_module.toml` | 25 Test Verification | E | partial |
| `add_adr.toml` | 21 Implement TODO Stub | D | partial |
| `debug_log_trace.toml` | 15 Fix Panic | C | full |
| `extract_module.toml` | 8 Extract Module | B | full |
| `extract_trait.toml` | 7 Extract Trait | B | full |
| `fix_borrow_error.toml` | 16 Resolve Borrow Checker Error | C | full |
| `fix_clippy_naming.toml` | 12 Fix Clippy Lints | C | full |
| `fix_clippy_warning.toml` | 12 Fix Clippy Lints | C | full |
| `fix_failing_test.toml` | 13 Fix Unit Test | C | full |
| `fix_lifetime_error.toml` | 16 Resolve Borrow Checker Error | C | partial |
| `inline_function.toml` | 10 Remove Duplication | B | partial |
| `multi_file_pattern.toml` | 31 Multi-file Feature | G | full |
| `pr_review.toml` | 34 PR Review | G | full |
| `refactor_extract_function.toml` | 10 Remove Duplication | B | full |
| `refactor_trait_extraction_multi.toml` | 7 Extract Trait | B | full |
| `rename_function.toml` | 6 Rename Public API | B | full |
| `rename_module.toml` | 6 Rename Public API | B | partial |
| `test_fix_cycle.toml` | 26 Self Repair | E | full |
| `use_budget_check.toml` | 30 Budget Enforcement | F | partial |
| `use_draw_render.toml` | 25 Test Verification | E | partial |
| `use_lsp_query.toml` | 28 Semantic Retrieval | F | partial |
| `use_stratum_compress.toml` | 29 Context Compression | F | full |
| `use_workflow_run.toml` | 31 Multi-file Feature | G | partial |
| `token_budget_challenge.toml` | 30 Budget Enforcement | F | full (signature) |

## Planned tasks

The following spec tasks are **not yet implemented**. Each is a future
workorder; the note names the feature it exercises.

| Spec task | Category | Exercises | Note |
|-----------|----------|-----------|------|
| 1 Find Dead Code | A | context index | tree-sitter symbol graph + unreferenced-symbol query |
| 2 Dependency Graph Accuracy | A | context index | crate-level dep graph generation |
| 3 Call Graph Generation | A | context index | per-symbol call graph |
| 4 Explain Module | A | context index | module summarisation without hallucination |
| 5 Cross-Repository Search | A | workspace support | trait-impl search across workspace |
| 9 Split Giant File | B | refactor | 2500-line file split |
| 18 Add REST Endpoint | D | non-Rust | needs a non-Rust task setup |
| 22 Build Verification | E | verifier bus | standalone build-verify task |
| 23 Formatter Verification | E | verifier bus | standalone fmt-verify task |
| 24 Lint Verification | E | verifier bus | standalone lint-verify task |
| 27 Large Repository Navigation | F | context index at scale | Linux-sized repo |
| 32 Large Refactor | G | multi-file | 50+ files |
| 33 Merge Conflict Resolution | G | git | realistic conflict resolution |
| 35 Regression Detection | G | verifier bus | PR regression prediction |
| 36 Token Efficiency | H | cost | standalone token-efficiency task |
| 37 Dollar Cost | H | cost | standalone cost task |
| 38 Time | H | cost | standalone latency task |
| 39 Retry Count | H | cost | standalone retry-count task |
| 40 Human Intervention | H | cost | standalone intervention task |

Honest deferral: the spec documents 40 tasks; this workorder (WO 14.7)
builds the signature one (`token_budget_challenge`) and maps the
existing 30. The remaining ~10 are future WOs.