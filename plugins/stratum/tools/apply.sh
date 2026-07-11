#!/usr/bin/env bash
# stratum_apply: apply the stratum pipeline to a file.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_apply: stratum binary not found (build the workspace or install stratum on PATH)"

ARGS="$(stratum_args)"

args=()

file="$(json_get_string "$ARGS" "file" "")"
content_type="$(json_get_string "$ARGS" "content_type" "")"
mode="$(json_get_string "$ARGS" "mode" "")"
token_budget="$(json_get_integer "$ARGS" "token_budget" "")"
json_out="$(json_get_bool "$ARGS" "json" "false")"
dry_run="$(json_get_bool "$ARGS" "dry_run" "false")"

if [ -z "$file" ]; then
  die "stratum_apply: missing required 'file' field; use stratum_run for inline text"
fi

[ -n "$content_type" ] && args+=("--content-type" "$content_type")
[ -n "$mode" ] && args+=("--mode" "$mode")
[ -n "$token_budget" ] && args+=("--token-budget" "$token_budget")
[ "$json_out" = "true" ] && args+=("--json")
[ "$dry_run" = "true" ] && args+=("--dry-run")

if [ ! -r "$file" ]; then
  die "stratum_apply: cannot read file: $file"
fi
exec "$STRATUM" "${args[@]}" apply "$file"
