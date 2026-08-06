# Headless JSON Output

**Source:** vix (`internal/headless/headless.go`)
**Goal:** Structured machine-parseable output mode for CI/CD integration. The `--non-interactive` flag already exists but only dumps raw text to stdout.

## Why

- CI pipelines need structured output (pass/fail, token cost, tool calls made)
- Tool call results are lost in text mode (only printed to stderr)
- JSON output enables post-processing: cost reports, action audits, test result parsing
- Stream-json mode enables real-time pipelines

## Output Modes

| Mode | Flag | Format |
|------|------|--------|
| Text (current) | `--non-interactive` | Raw text, tokens to stdout |
| JSON | `--non-interactive --output json` | Single JSON object at end |
| Stream JSON | `--non-interactive --output stream-json` | One JSON line per event |

## JSON Output Schema

```json
{
    "version": "1.0",
    "session": {
        "id": "2026-06-03-session-01",
        "model": "qwen2.5:0.5b",
        "started_at": "2026-06-03T14:00:00Z",
        "duration_ms": 45200
    },
    "messages": [
        {
            "role": "user",
            "content": "Fix the bug in src/main.rs"
        },
        {
            "role": "assistant",
            "content": "I found the issue in...",
            "tokens": 342
        },
        {
            "role": "tool",
            "name": "read_file",
            "output": "...",
            "truncated": false
        }
    ],
    "tool_calls": [
        {
            "name": "read_file",
            "arguments": {"path": "src/main.rs"},
            "result": "success",
            "duration_ms": 12
        },
        {
            "name": "edit_file",
            "arguments": {"path": "src/main.rs", "old_string": "..."},
            "result": "success",
            "duration_ms": 8
        }
    ],
    "usage": {
        "prompt_tokens": 1250,
        "completion_tokens": 4800,
        "total_tokens": 6050,
        "cost_usd": 0.0
    },
    "verdict": "pass",
    "error": null
}
```

## Stream JSON (per-event)

```json
{"type": "token", "content": "I'll fix the bug by..."}
{"type": "tool_call", "name": "read_file", "arguments": {"path": "src/main.rs"}}
{"type": "tool_result", "name": "read_file", "duration_ms": 12}
{"type": "done", "usage": {"prompt_tokens": 1250, "completion_tokens": 4800, "cost_usd": 0.0}}
```

## Integration Points

| File | Change |
|------|--------|
| `src/main.rs` | Add `--output text|json|stream-json` to Cli |
| `src/main.rs` | In `run_non_interactive`, switch output format based on flag |
| `src/shared/mod.rs` | New `OutputFormat` enum |

## Token Cost in CI

Combine with cost tracking: `--max-cost 0.05` to cap spending. In CI, if the budget is exceeded, exit code 75 (EX_TEMPFAIL) so the CI system can retry or skip.