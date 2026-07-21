# ADR-034: Mid-batch tool-result checkpointing

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

`dispatch_tool_call_batch` writes a checkpoint only after the entire tool batch completes (`turn.rs:264`). If the process crashes while a batch is in progress, any tool results that have already finished and been appended to the conversation are held only in memory; on restart the conversation is restored to the state before the batch started and the completed work is lost.

The ChatGPT grade criterion expects recovery to the last completed step rather than the last completed turn: "Step 14 of 18 → power failure → resume automatically." Per-result checkpointing closes that gap for the tool-dispatch phase without changing the existing post-batch checkpoint semantics.

## Decision

After each tool result is recorded in Phase 3 of `dispatch_tool_call_batch`, call `self.conversation.checkpoint_async().await`. The existing post-batch checkpoint at `turn.rs:264` is preserved as the final durability guarantee.

Key points:

- Checkpointing happens **after** the result is appended to the conversation so the in-memory message list and the checkpoint are consistent.
- It runs inside the sequential Phase 3 loop, not inside the spawned Phase 2 tasks, so ordering and executor-state mutations remain single-threaded.
- Failures are logged and emitted as `TurnEvent::Error` but do not abort the batch, matching the existing post-batch checkpoint error handling.
- The post-batch checkpoint is unchanged; the new per-result checkpoints only narrow the recovery window during the batch itself.

## Consequences

- A crash mid-batch now loses at most the most recently started unrecorded tool instead of the whole batch.
- More checkpoint files are written during a multi-tool turn. The existing `MAX_CHECKPOINTS = 5` prune logic keeps disk usage bounded, and nanosecond timestamps prevent collisions.
- Record phase latency increases slightly because each result is followed by a synchronous (within the async task) disk copy. The dominant cost remains the tool body itself for real tools.
- Tests can simulate a crash by aborting the turn task and verify that the restored conversation contains exactly the recorded subset of results.
