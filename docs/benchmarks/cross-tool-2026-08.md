# Cross-Tool Benchmark — August 2026 (WO 32.6)

*Template only — results not yet filled in. The runner script
(`scripts/run-cross-tool-bench.sh`) produces the raw JSON; this file
holds the analysis + comparison table the operator writes from that data.*

## Thesis

KirkForge (kf-code) is more **context-efficient** than Codex / Claude
Code: it completes the same task using fewer tokens and fewer turns as
the context budget shrinks, because the budget-stratum compression keeps
the conversation in-window where competitors lose context or refuse.

## Method

1. **Tasks** — 4 representative tasks (bug fix, feature add, refactor, docs).
2. **Budgets** — 5 context ceilings per task: 128k / 64k / 32k / 16k / 8k.
3. **Tools** — kf-code (automated via `scripts/run-cross-tool-bench.sh`),
   Codex CLI (manual), Claude Code (manual).
4. **Metrics** — `tokens_consumed`, `turns_taken`, `success`, `wall_clock_secs`.
5. **Format** — JSON `ExternalToolReport` batches (see `kf_bench::ExternalToolReportBatch`).

## Tasks

| Name | Difficulty | Prompt |
|------|-----------|--------|
| bug-fix | medium | Fix the failing test in `src/lib.rs`: the `add` function panics on negative input. |
| feature-add | medium | Add a `--version` flag to the CLI that prints the version and exits. |
| refactor | hard | Extract the duplicated token-counting logic in `parser.rs` into a shared helper. |
| docs | easy | Write a README section documenting the bench harness usage and task format. |

## Raw data

Raw JSON reports live in `docs/benchmarks/out/` (gitignored — large +
per-run). Each file is an `ExternalToolReportBatch` with one
`ExternalToolReport` per (task, budget, tool) combination.

## Results

<!-- Fill in after running scripts/run-cross-tool-bench.sh and the manual external runs. -->

### Success rate by tool × budget

| Tool | 128k | 64k | 32k | 16k | 8k |
|------|------|-----|-----|-----|-----|
| kf-code | — | — | — | — | — |
| codex | — | — | — | — | — |
| claude-code | — | — | — | — | — |

### Tokens consumed (median across runs)

| Tool | 128k | 64k | 32k | 16k | 8k |
|------|------|-----|-----|-----|-----|
| kf-code | — | — | — | — | — |
| codex | — | — | — | — | — |
| claude-code | — | — | — | — | — |

### Turns taken

| Tool | 128k | 64k | 32k | 16k | 8k |
|------|------|-----|-----|-----|-----|
| kf-code | — | — | — | — | — |
| codex | — | — | — | — | — |
| claude-code | — | — | — | — | — |

### Wall-clock (seconds)

| Tool | 128k | 64k | 32k | 16k | 8k |
|------|------|-----|-----|-----|-----|
| kf-code | — | — | — | — | — |
| codex | — | — | — | — | — |
| claude-code | — | — | — | — | — |

## Analysis

<!-- Written after the data is collected. -->

## Verdict

<!-- Either "thesis validated" or "thesis refuted" with a one-paragraph
justification grounded in the tables above. -->

## Notes

- External-tool runs are manual because they require separate installs
  and credentials. The runner script emits an empty template JSON for
  Codex/Claude Code that the operator fills in.
- `tokens_consumed` / `turns_taken` for kf-code are read from the
  kf-code bench report JSON (`BenchReport`) written alongside the
  cross-tool JSON; the script defaults them to 0 as a placeholder.