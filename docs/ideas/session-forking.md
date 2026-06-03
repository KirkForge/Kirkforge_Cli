# Session Forking

**Source:** vix (`internal/daemon/session.go` turnSnapshots, ForkSessionID, ForkTurnIdx)
**Goal:** Fork a session's conversation history at an arbitrary turn. Enables branching workflows: explore two approaches from the same starting point without re-running the first N turns.

## Why

- LLM conversations are linear. A bad turn means starting over.
- With forking, you fork at the last good turn, try the alternative, and compare.
- The stem agent pattern (workflow engine) uses forking internally: each step forks from the prior step's conversation, sharing the system prompt for prompt cache hits.
- Users can explore "what if I used a different approach" without losing the original thread.

## API

```rust
pub struct ForkPoint {
    pub session_id: String,
    pub turn_index: usize,    // copy messages[0..turn_index]
}

impl ConversationLog {
    /// Fork a new conversation log from an existing one at a given turn index.
    pub fn fork(&self, turn_index: usize) -> Result<ConversationLog> {
        let history = &self.messages[..turn_index.min(self.messages.len())];
        let new_path = self.path.with_file_name(format!(
            "fork-{}-{}.conv.ndjson",
            turn_index,
            chrono::Local::now().format("%H%M%S")
        ));
        let mut forked = ConversationLog::open(new_path)?;
        for msg in history {
            forked.append(msg.clone())?;
        }
        Ok(forked)
    }
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/session/conversation.rs` | Add `fork(turn_index)` method |
| `src/tui/mod.rs` | Add `/fork <turn>` slash command |
| `src/session/executor.rs` | Track turn index in executor (emit as TurnEvent or track internally) |

## UI

In the TUI, `/fork 5` creates a new conversation log that copies messages 0-5 from the current session. The user can then continue from that point. The original conversation is still accessible via `--resume`.

In the workflow engine, `fork_from` is a declarative way to express the same pattern: `step B forks from step A` means step B's conversation starts with all of step A's history.