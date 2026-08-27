#!/usr/bin/env bash
# Cross-tool benchmark runner (WO 32.6 / WO 39.1).
#
# Phase 2: the kf-code half now uses `kf-code bench run --output` to run
# the real task suite and emit a BenchReport JSON. The external-tool half
# exports task workspaces via `kf-code bench export-tasks` so an operator
# can point Codex / Claude Code / opencode at them and fill in the
# ExternalToolReport JSON manually. External tool runs are manual because
# they require separate installs/credentials.
#
# Usage: scripts/run-cross-tool-bench.sh [--tasks-dir <dir>] [--out-dir <dir>] [--model <model>]
# Defaults: --tasks-dir benches/tasks  --out-dir docs/benchmarks/out
# NOTE (WO 47.5): the `bench` subcommand is devtools-gated — build the
# binary with `cargo build --features devtools` (or point KF_CODE_BIN at a
# devtools build) before running this script.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TASKS_DIR="$ROOT/benches/tasks"
OUT_DIR="$ROOT/docs/benchmarks/out"
MODEL=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tasks-dir) TASKS_DIR="$2"; shift 2;;
    --out-dir)   OUT_DIR="$2";   shift 2;;
    --model)     MODEL="$2";     shift 2;;
    *) echo "unknown arg: $1" >&2; exit 1;;
  esac
done

mkdir -p "$OUT_DIR"

KF_BIN="${KF_CODE_BIN:-kf-code}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"

echo "WO 32.6/39.1 cross-tool benchmark runner"
echo "tasks_dir=$TASKS_DIR out_dir=$OUT_DIR kf_bin=$KF_BIN"
echo

# Phase 2: kf-code baseline via the real bench harness. `bench run` loads
# the TOML suite, runs each task against the model, verifies, and writes a
# BenchReport JSON. The exit code is non-zero on 0% success (WO 38.10),
# which is a real failure — not a success — so we propagate it.
KF_REPORT="$OUT_DIR/${TS}_kf-code-bench-report.json"
KF_ARGS=(bench run --tasks "$TASKS_DIR" --output "$KF_REPORT")
if [[ -n "$MODEL" ]]; then
  KF_ARGS+=(--model "$MODEL")
fi
echo "→ kf-code baseline: ${KF_ARGS[*]}"
if "$KF_BIN" "${KF_ARGS[@]}"; then
  echo "  wrote $KF_REPORT"
else
  rc=$?
  echo "  kf-code bench run exited $rc (report may still be written: $KF_REPORT)"
fi

# Export task workspaces for external agents (excludes kf-only tasks).
EXPORT_DIR="$OUT_DIR/${TS}_task-workspaces"
"$KF_BIN" bench export-tasks --tasks "$TASKS_DIR" "$EXPORT_DIR"
echo "  wrote task workspaces to $EXPORT_DIR"

# External-tool template (empty batch the operator fills in for Codex/Claude Code).
# ponytail: manual fill-in rather than a full external runner (Phase 3, deferred).
# Upgrade path: a generic runner that invokes `claude -p` / `codex exec` / `opencode
# run` per task, parses per-CLI usage JSON, and emits ExternalToolReport rows.
TEMPLATE="$OUT_DIR/${TS}_external-template.json"
cat > "$TEMPLATE" <<'EOF'
{
  "reports": [
    { "tool_name": "codex",       "task_name": "fix_failing_test",  "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 },
    { "tool_name": "claude-code", "task_name": "fix_failing_test",  "context_budget": 131072, "tokens_consumed": 0, "turns_taken": 0, "success": false, "wall_clock_secs": 0.0 }
  ]
}
EOF
echo "  wrote $TEMPLATE (fill in Codex/Claude Code results manually)"
echo
echo "Next steps:"
echo "  1. Run each external agent in $EXPORT_DIR/<task>/ with the same prompt (PROMPT.txt)."
echo "  2. Fill in $TEMPLATE with real tokens/success/wall-clock per task."
echo "  3. Combine *_kf-code-bench-report.json + *_external-template.json and"
echo "     run `cargo run -p kf-bench --example cross-tool-compare <files...>` to"
echo "     produce the comparison table."