# ADR-032: Exponential Backoff on Tool-Call Retries

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

Model API retries in `adapters/mod.rs` already use exponential backoff (1 s, 2 s, 4 s) with deterministic jitter for connect/timeout errors and transient HTTP statuses. Tool-call retries, however, were immediate. `RetryTracker` in `error_recovery.rs` counted parse-error retries but slept for zero time, which did not match the "retries with exponential backoff on every tool invocation" requirement.

## Decision

1. Promote the existing `retry_backoff(attempt)` helper from `adapters/mod.rs` into a shared module (`src/shared/backoff.rs`) and re-export it from `adapters/mod.rs`.
2. Add an async `RetryTracker::wait_before_retry()` method that sleeps for `retry_backoff(retry_count + 1)`.
3. In the executor turn loop (`src/session/executor/turn.rs`), call `wait_before_retry()` and then `record_retry()` before each parse-error retry.
4. Keep the deterministic jitter policy (up to 250 ms per attempt, capped at 1 s) so behavior is stable in tests without adding a random-source dependency.

## Consequences

- Tool-call retries now share one backoff policy with model-request retries.
- The first retry waits ~1 s, the second ~2 s, the third ~4 s, giving the upstream service time to recover.
- Tests can assert real elapsed time with small tolerances because jitter is deterministic.
