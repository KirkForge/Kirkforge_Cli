#!/usr/bin/env bash
# Cross-tool benchmark runner (WO 32.6).
#
# Runs kf-code on representative tasks at 5 context budgets (128k/64k/32k/16k/8k),
# records results as JSON in docs/benchmarks/out/, and emits a template JSON
# file for external tools (Codex, Claude Code) so the operator can fill them in
# manually. External tool runs are manual because they require separate
# installs/credentials — this script just produces the kf-code half + scaffolding.
#
# Usage: scripts/run-cross-tool-bench.sh [--tasks-dir <dir>] [--out-dir <dir>]
# Defaults: --tasks-dir crates/kf-bench/tasks  --out-dir docs/benchmarks/out
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TASKS_DIR="$ROOT/crates/kf-bench/tasks"
OUT_DIR="$ROOT/docs/benchmarks/out"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tasks-dir) TASKS_DIR="$2"; shift 2;;
    --out-dir)   OUT_DIR="$2";   shift 2;;
    *) echo "unknown arg: $1" >&2; exit 1;;
  esac
done

mkdir -p "$OUT_DIR"

# 5 context budgets (tokens). 128k is the widest, 8k the tightest.
BUDGETS=(131072 65536 32768 16384 8192)

# Representative tasks (bug fix, feature add, refactor, docs). Each entry is
# "name|prompt". The task files live in $TASKS_DIR as TOML; this script assumes
# the operator has authored them. See docs/workorders/32.6-cross-tool-benchmark.md.
TASKS=(
  "bug-fix|Fix the failing test in src/lib.rs: the add function panics on negative input."
  "feature-add|Add a --version flag to the CLI that prints the version and exits."
  "refactor|Extract the duplicated token-counting logic in parser.rs into a shared helper."
  "docs|Write a README section documenting the bench harness usage and task format."
)

KF_BIN="${KF_CODE_BIN:-kf-code}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"

echo "WO 32.6 cross-tool benchmark runner"
echo "tasks_dir=$TASKS_DIR out_dir=$OUT_DIR kf_bin=$KF_BIN"
echo

# kf-code reports (one JSON file per task, containing one ExternalToolReport per budget).
for entry in "${TASKS[@]}"; do
  IFS='|' read -r task_name prompt <<< "$entry"
  report_file="$OUT_DIR/${TS}_${task_name}_kf-code.json"
  reports_json="[]"
  for budget in "${BUDGETS[@]}"; do
    echo "→ kf-code | $task_name | budget=$budget"
    # Run kf-code non-interactively with the budget ceiling pinned. The bench
    # harness writes a BenchReport JSON; we convert the relevant fields into
    # ExternalToolReport via the kf-bench CLI (or jq if kf-bench lacks a converter).
    # ponytail: shell-level timing is a coarse ceiling; the kf-bench crate
    # records precise per-turn tokens/duration in its own BenchReport. Upgrade
    # path: a `bench cross-tool export` subcommand that emits ExternalToolReport
    # JSON directly so this script doesn't hand-roll jq.
    start=$(date +%s.%N)
    if KF_CODE_BUDGET_CEILING="$budget" "$KF_BIN" --non-interactive --prompt "$prompt" --no-tui \
        >/dev/null 2>&1; then
      success="true"
    else
      success="false"
    fi
    end=$(date +%s.%N)
    wall=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.1f", e - s}')
    # tokens_consumed/turns_taken are best-effort from the kf-code log; operator
    # fills the precise values from $OUT_DIR/<task>_bench_report.json. Default 0
    # so the JSON is always valid; the comparison table renders 0 as a placeholder.
    reports_json=$(echo "$reports_json" | jq --arg tool "kf-code" \
      --arg task "$task_name" --argjson budget "$budget" --argjson success "$success" \
      --argjson wall "$wall" \
      '. + [{tool_name:$tool, task_name:$task, context_budget:$budget, tokens_consumed:0, turns_taken:0, success:$success, wall_clock_secs:$wall}]')
  done
  echo "{\"reports\":$reports_json}" > "$report_file"
  echo "  wrote $report_file"
done

# External-tool template (empty batch the operator fills in for Codex/Claude Code).
TEMPLATE="$OUT_DIR/${TS}_external-template.json"
cat > "$TEMPLATE" <<'EOF'
{
  "reports": [
    { "tool_name": "codex",       "task_name": "bug-fix",     "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "codex",       "task_name": "bug-fix",     "context_budget": 65536,  "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "codex",       "task_name": "bug-fix",     "context_budget": 32768,  "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "codex",       "task_name": "bug-fix",     "context_budget": 16384,  "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "codex",       "task_name": "bug-fix",     "context_budget": 8192,   "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "claude-code", "task_name": "bug-fix",     "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "claude-code", "task_name": "feature-add", "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "claude-code", "task_name": "refactor",    "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "claude-code", "task_name": "docs",        "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 }
  ]
}
EOF
echo "  wrote $TEMPLATE (fill in Codex/Claude Code results manually)"
echo
echo "Done. Combine the *_kf-code.json + *_external-template.json files and"
echo "run `cargo run -p kf-bench --example cross-tool-compare <files...>` to"
echo "produce the comparison table (or load them via kf_bench::load_external_reports)."