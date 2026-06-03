# ADR 004: Tool Use — Client-Side Tool Dispatch with Approval Gates

## Status

Accepted

## Date

2026-06-03

## Context

The agent needs tools: read_file, write_file, edit_file, bash, grep, glob. These are the primitives that let it understand, modify, and verify code. ADR 003 defined how tool calls arrive as `StreamEvent::ToolCall` from any model. But arrival is not execution — we need to decide:

1. **Who interprets tool calls?** The model emits JSON tool call blocks. The client needs to parse them, validate them against a schema, execute them, and return results.
2. **Who approves execution?** Bash and write_file are destructive. Running them without oversight on the C-50 where the user might be looking away is bad. But requiring approval for every read_file is annoying.
3. **How do results flow back?** Synchronously (block the stream until the tool finishes) or asynchronously (insert results as they arrive)?

## Decision

Client-side tool dispatch with a tiered approval system and sync-by-default execution.

### Tool registry

All tools are defined in a `Tool` enum with a name, description, JSON schema for arguments, and a `run` function:

```rust
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value, // JSON Schema
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn run(&self, args: serde_json::Value) -> ToolResult;
}

pub enum ToolResult {
    Success { content: String },
    Error { message: String, code: ErrorCode },
    // File reads and greps return structured data the UI can display natively,
    // not just text blobs. The bash tool returns plain text.
    FileContent { path: PathBuf, content: String, truncated: bool },
    FileEdit { path: PathBuf, diff: String },
    GrepMatches { path: PathBuf, matches: Vec<Match>, total: usize },
}
```

Built-in tools:

| Tool | Arguments | Destructive | Streaming |
|------|-----------|-------------|-----------|
| `read_file` | path, offset, limit | No | No |
| `write_file` | path, content | Yes | No |
| `edit_file` | path, old_string, new_string | Yes | No |
| `bash` | command, timeout, workdir | Yes | Yes (stdout/stderr) |
| `grep` | pattern, path, context_lines | No | No |
| `glob` | pattern, base_dir | No | No |

### Approval tiers

```
ReadOnly  → auto-approve (read_file, grep, glob)
          → unless path matches a .env, .git-credentials, or ~/.ssh/* pattern

Destructive → require explicit approval per invocation
  (write_file, edit_file, bash) except:
  - bash read-only commands (ls, cat, head, tail, grep — detected by AST of the command)
  - Undo: if a destructive tool just ran and this is the reversal, match on diff
```

Approval is a prompt in the TUI — Y/n with context showing the exact change. The session blocks on that tool call until the user responds. A `--yes` / `--auto` flag on startup disables approval gates for automated use (CI, overnight runs).

### Execution flow

```
1. Model emits ToolCall(name=write_file, args={path, content})
2. Session layer validates args against ToolDef.parameters
3. Session checks approval tier → Destructive → pause stream, show prompt
4. User approves → Tool.run() executes
5. Result is inserted as a new message in conversation history (role: "tool")
6. Session sends a synthetic prompt asking the model to continue
7. Model either continues generating or emits the next tool call
```

The stream from the model is paused during tool execution. The model doesn't receive the tool result until step 5. This is synchronous per tool call — the model cannot issue multiple concurrent tool calls and expect parallel execution (DeepSeek sometimes sends multiple tool calls in one response). For the batch case: execute them sequentially in order, collect results, send them all back as one turn.

### Bash execution

`bash` runs through `std::process::Command` with:
- Timeout (default 30s, configurable per invocation)
- Working directory (defaults to project root)
- No PTY — captures stdout/stderr as strings
- Kill on timeout, return partial output + timeout error
- `SHELL` env blocked; uses `/bin/sh` always (no bashisms that break on the P30's BusyBox)

### File editing

`edit_file` implements Vix's approach: find exact string match, replace. This is simpler and more reliable than line-number-based edits (which drift when the model miscounts) or sed-like patches (which have escaping edge cases). The tool returns a unified diff for the approval prompt.

```rust
fn edit_file(path, old_string, new_string) -> Result {
    let content = fs::read_to_string(&path)?;
    if !content.contains(&old_string) {
        // Try fuzzy match: strip whitespace, compare normalized
        // If still not found, return error with context lines
        return Err(ToolError::StringNotFound { path, context_lines });
    }
    let new_content = content.replace(&old_string, &new_string);
    // Compute diff for display
    let diff = diff(&content, &new_content);
    fs::write(&path, &new_content)?;
    Ok(ToolEdit { path, diff })
}
```

## Consequences

**Positive:**
- Read operations are frictionless. No approval prompt for every file scan.
- Destructive operations have an audit trail. The diff is shown before execution.
- Tool definition is decoupled from both the model adapter and the UI. A new tool is one file implementing `Tool`.
- Bash auto-detection of read-only commands saves approval fatigue without weakening the gate.

**Negative:**
- Synchronous tool execution blocks the stream. For long-running bash commands (test suites, builds), the model sits idle. Mitigation: increase the bash timeout default to 120s for known-long commands, and add a `--background` flag for fire-and-forget scripts.
- Sequential DeepSeek batch tool calls add latency. If the model emits 3 tool calls and tool 1 is a write that takes 1ms while tool 2 is a read that takes 1ms, the tool calls are still serialized. Mitigation: identify independent tool calls (no overlapping paths, no read-after-write to same file) and execute them in parallel. Add this post-MVP.
- Fuzzy edit matching is heuristic. It might replace the wrong occurrence. Mitigation: the approval prompt shows the diff — the user catches it before it runs.

## Open Questions

- Should bash output be streamed to the UI in real-time (like a terminal) or delivered as a block after completion? Streaming is more responsive but requires more TUI state. Start with block delivery, add streaming as a UI refinement.
- Tool call context window management: each tool result is a full `{role: "tool", content: "..."}` message. A `cat` of a large file or a `grep` with 500 matches blows the context. Mitigation: truncate tool results at a configurable limit and append `\n... (N more lines truncated)`. Make this a per-tool setting.