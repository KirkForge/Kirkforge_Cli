# ADR 005: Session Management and Prompt Construction

## Status

Accepted

## Date

2026-06-03

## Context

The conversation between user and model is the core data structure. It must be:

- **Persistent** — survive crashes, restarts, deliberate pauses. Network hiccups, cloud API rate limits, and host sleep/wake cycles should not lose the conversation.
- **Resumable** — reconnect to a different model mid-conversation if one model is unavailable. Switch from GLM to DeepSeek without losing context.
- **Token-aware** — the user runs three models with different context windows. Prompt construction must budget tokens per model and trim strategically when the window fills.
- **Replayable** — for debugging, reproducing bugs, and auditing what the agent was told.

## Decision

A simple append-only log format for the conversation, with token-budgeted prompt construction and a system prompt template that's model-type-aware.

### Conversation format

Flat file, one event per line, newline-delimited JSON (NDJSON):

```
logs/2026-06-03-session-01.conv.ndjson
```

```jsonl
{"ts": "2026-06-03T10:00:00Z", "role": "system", "content": "You are an AI coding agent. Available tools: ..."}
{"ts": "2026-06-03T10:00:01Z", "role": "user", "content": "list all files in src/"}
{"ts": "2026-06-03T10:00:02Z", "role": "assistant", "content": "I'll read the directory structure.", "thinking": "User wants a file listing..."}
{"ts": "2026-06-03T10:00:03Z", "role": "tool", "name": "glob", "args": {"pattern": "src/**/*"}, "result": "src/main.rs\nsrc/lib.rs"}
{"ts": "2026-06-03T10:00:04Z", "role": "assistant", "content": "Here's the source tree: ..."}
```

Benefits:
- `tail -f` for debugging. `grep` for finding when a tool was called. `wc -l` for message count.
- Append-only means no corruption on crash — at most lose the last partial line.
- Can replay by reading the file and feeding each event back through a model adapter.
- Easy to rotate: move the file, start a new one.

### Session lifecycle

```
Sessions live in ~/.local/share/ollama-cli/sessions/
├── active/          → symlink or named pipe to current session
├── archive/         → compressed after 7 days or 10k messages
└── config.toml      → default model, auto-approve, theme, keybindings
```

`config.toml` (minimal):

```toml
# Defaults are empty. Set these to the Ollama gateway that routes your
# chosen frontier model, e.g. a cloud provider endpoint.
default_model = ""
ollama_host = ""
auto_approve = false
truncation_strategy = "drop_oldest"  # drop_oldest | summarize_middle | keep_tools_only
max_tool_result_chars = 4000
# Map per-tier models to full provider names:
# routing_model_map = { complex = "kimi-2.7k-coder:cloud", medium = "glm-5.2:cloud", simple = "qwen3:32b:cloud" }
```

### Prompt construction

Each model has a different context window. GLM-5.2 claims 128K, DeepSeek-v4-Pro claims 64K, Gemini 3.0 Flash 1M claims 1M, Kimi-2.7k-Coder:Cloud claims 256K. The prompt builder takes the full conversation log and produces the message array sent to the model:

```
1. Start with system prompt (always included, counted against budget)
2. Build message list from the conversation log, newest-first
3. Stop adding messages when budget is exhausted (with safety margin)
4. If the oldest message being dropped is a tool result that the model
   still references, prefer dropping a different message or summarizing
5. Return message list in chronological order (reversed back)
```

Truncation strategies:
- `drop_oldest` — drop from the top of history until the budget fits. Simplest. Risks losing early context like "this project is called X."
- `keep_tool_only` — drop user and assistant text messages before dropping tool results. Preserves the tool output the model might reference.
- `summarize_middle` — when dropping would lose too much, insert a synthetic "earlier in this conversation, user asked about X and we determined Y" message. Mark it with `role: "system"` and a `[summarized]` prefix so the model treats it as background context, not verbatim history.

Start with `drop_oldest`. Add `summarize_middle` if context loss becomes a visible problem.

### System prompt template

The system prompt is assembled from parts, not a static string. Each part is optional and gated on whether it applies:

```
You are an AI coding agent running in a terminal. You have access to tools.

{{#if current_file}}
The user is currently viewing: {{current_file}}
{{/if}}

{{#if model_supports_thinking}}
Use your thinking capacity to reason about the problem before responding.
Your thinking will be shown to the user in a collapsible panel — use it freely.
{{/if}}

Available tools:
{{#each tools}}
- {{name}}: {{description}}
  Arguments: {{parameters}}
{{/each}}

Guidelines:
- Prefer edit_file over write_file for small changes — it preserves surrounding context
- Run bash commands to verify changes before declaring them done
- When a tool returns an error, read the error carefully before retrying
- If unsure, use read_file to check the current state
```

The template lives in `~/.local/share/ollama-cli/prompts/system.hbs` (Handlebars-style, but rendered in Rust with a simple template engine or string replacement — no heavy dependency). Users can override it.

### Prompt compression (VFS insight)

ADR 001's milestone 3 calls for "syntax-aware minification." The insight from Vix: code can be minified before sending to the model, because the model understands the semantics from the structure alone. Whitespace, dead comments, and verbose identifiers are token-cost waste.

Implementation — a pre-processor that runs before the prompt is assembled:

```rust
pub fn minify_source(path: &Path, content: &str, language: &str) -> String {
    match language {
        "rust" => minify_rust(content),   // strip comments, compress whitespace,
                                          // shorten let bindings
        "python" => minify_python(content), // strip comments/docstrings, join lines
        "javascript" => minify_js(content), // short-rename local identifiers
        _ => content, // unknown language, return as-is
    }
}
```

This is NOT a full parser-based minifier — that would add tree-sitter as a dependency and slow down every prompt build. It's a heuristic:

1. Strip single-line comments (`//`, `#`)
2. Strip doc comments (`///`, `/** */`, `"""` — the model doesn't need the docs it just generated)
3. Collapse consecutive blank lines to one
4. If the token budget is still exceeded, strip block comments

This is applied at prompt-build time, not at file-read time. The file on disk is never modified. The TUI shows the original file. Only the model sees the compressed version.

## Consequences

**Positive:**
- Append-only NDJSON is crash-safe. A power cut loses at most one partial line.
- Prompt construction is deterministic and testable — given a conversation log and a token budget, the output message array is predictable.
- System prompt is user-customizable without recompiling.
- Minification at prompt-build time saves tokens without touching the user's files.
- Session replay is a debugging superpower — `cat session.conv | jq` shows exactly what was sent.

**Negative:**
- NDJSON is not indexed. Searching across sessions requires grep or a separate tool. Mitigation: sessions are short-lived (rotated daily or per-project), so each file is small enough to grep.
- `drop_oldest` truncation loses early context. If the user sets a project description in message 5 and the conversation runs 200 messages deep, message 5 is the first to go. Mitigation: start with `keep_tool_only` as the default instead of `drop_oldest` — it preserves the structured context (tool results) and drops the chatty parts.
- Minification by stripping comments is lossy. If the model previously wrote a comment explaining why a complex algorithm works, and then the file gets read back in a later turn with comments stripped, the model might not understand the code it wrote. Mitigation: minification only applies to tokens approaching the budget limit — if under budget, send the original content verbatim.
- The minifier is language-dependent. Adding a new language means adding a minification function. Mitigation: start with Rust-only (the project's own language), extend as needed.

## Open Questions

- Should the session log include token counts per message? It would make truncation decisions more precise (you can see exactly how many tokens "message 12" costs without re-tokenizing). Add a `tokens` field to each JSON line, computed at insert time using a tokenizer (tiktoken-rs or similar). Worth the extra cache field.
- Should the VFS minification cache results? If the same file is read across multiple turns, minifying it each time is wasted CPU. Cache the minified output keyed by (path, mtime). Invalidate on file change.