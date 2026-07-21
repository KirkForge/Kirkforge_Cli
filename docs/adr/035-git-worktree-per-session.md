# ADR-035: Git Worktree Per Session

**Status:** Accepted (2026-07-21)

## Context

ChatGPT's cross-review named "Docker execution, ephemeral workspaces, Git worktrees, resource limits" as the sandboxing bar. `grep -rn 'docker|seccomp|rlimit|cgroup|setrlimit|namespace' src/` → 0 hits. The sandbox was path-based (filesystem + network deny-list + permission rules), not process-isolation-based.

Without ephemeral workspaces, edits land directly in the user's working tree. A model that writes garbage or deletes files does so in the user's repo, not an isolated copy.

## Decision

Add a `--worktree` flag that creates an isolated git worktree for the session:

1. On session start, `git worktree add --detach /tmp/kirkforge-session-<id> HEAD` creates a detached worktree at a temp path.
2. The sandbox directory is redirected to the worktree path, so all file tools operate inside the worktree.
3. On session end (Drop), `git worktree remove --force /tmp/kirkforge-session-<id>` cleans up.

## Implementation

- `src/cli.rs`: `#[arg(long)] worktree: bool` on the `Run` variant.
- `src/shared/mod.rs`: `pub worktree_enabled: bool` on `Config`, default `false`.
- `src/session/worktree.rs`: `WorktreeSession` struct with `create()` and `Drop`.
- `src/main/mod.rs`: after sandbox freeze, if `worktree_enabled`, create worktree and redirect `sandbox_dir`.

## Consequences

**Positive:**
- Edits land in an isolated worktree, not the user's working tree.
- No Docker dependency — works on any system with git.
- Cleanup is automatic via Drop.

**Negative:**
- Only works inside a git repository (the common case for coding agents).
- The worktree is a full checkout — for large repos this adds disk usage.
- Background bash jobs still run in the host namespace (not containerized).
