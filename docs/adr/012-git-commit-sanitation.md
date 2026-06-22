# ADR 012: Safe Git Commit Helper — `/commit` with Pre-Commit Sanitation

## Status

Accepted

## Date

2026-06-22

## Context

KirkForge runs as a coding assistant inside a git worktree. A common failure mode is asking the model to "commit and push" and getting:

- a bad or empty commit message,
- accidentally committed secrets or large binaries,
- merge-conflict markers committed,
- or an unexpected push to the wrong remote.

The shell is available via the `bash` tool, but giving the model unrestricted `git commit -a -m ...` is risky. We want a first-class slash command that reviews the working tree before committing and optionally pushes after a successful commit.

## Decision

Implement `/commit` as a TUI slash command backed by a deterministic pre-commit sanitation pass.

### Command surface

- `/commit` — runs sanitation, prints `git status`, and suggests a conventional-commit style message. Does **not** commit.
- `/commit "message"` — runs sanitation, stages all changes (`git add -A`), commits with the message.
- `/commit --push "message"` — commits and then runs `git push`.

Sanitation is **fail-closed**: blockers abort the commit. Warnings are shown but do not block.

### Sanitation checks

Implemented in `src/session/git_sanitation.rs`:

1. **Large files** — files larger than `commit_max_file_size` (default 5 MiB) are blocked. This catches accidentally committed binaries, dumps, and bundles.
2. **Secret / credential patterns** — substring scan for `ghp_`, `github_pat_`, `sk-`, `glpat-`, `id_rsa`, `id_ed25519`, `.env`, private-key PEM headers, and `AKIA` (AWS access key id). Case-insensitive.
3. **Merge-conflict markers** — lines starting with `<<<<<<< `, `=======`, or `>>>>>>> ` block the commit.
4. **Untracked / unstaged debris** — untracked files and unstaged modifications are reported as warnings so the user knows `git add -A` will stage them.

All checks run against `git status --porcelain` output; no LLM round-trip is required, so results are deterministic and fast.

### Config

`commit_max_file_size: u64` in `Config` controls the large-file threshold. Default: 5 MiB. Omitted keys fall back to the default thanks to serde.

### Suggested message

`suggest_message` examines the changed files and proposes a conventional-commit style message:

- `test(...)` if any file name contains "test".
- `docs(...)` if only documentation changed.
- `feat(...)` if Rust source files changed.
- `chore(...)` otherwise.

This is intentionally simple; a future pass can ask the model for a richer message.

### Integration

- `src/tui/commands/commit.rs` implements the handler and git subprocess calls.
- `src/tui/keys.rs` wires `/commit` into the slash-command dispatcher.
- `src/session/verifier/security.rs` already scans `FileWrite` and `Edit` events for secrets, providing an automatic second layer outside the explicit `/commit` path.

## Consequences

**Positive:**
- Gives the model a safe, reviewable path to commit without dropping to raw shell.
- Prevents the most common accidental bad commits (secrets, huge files, conflict markers).
- Configurable threshold lets operators tune the large-file limit for their domain.
- Optional `--push` keeps the user in control of whether to publish.

**Negative / limitations:**
- Secret detection is substring-based and can miss obfuscated secrets or flag false positives (e.g., `sk-` inside prose). It is a safety net, not a guarantee.
- The suggested message is keyword-driven and may be generic for mixed changes.
- `/commit` always stages with `git add -A`; partial staging is not supported.
- Push is unconditional `git push` with no dry-run or branch confirmation.

## Implementation

- `src/session/git_sanitation.rs` — core checks and report formatting.
- `src/tui/commands/commit.rs` — `/commit` handler, argument parsing, and git subprocesses.
- `src/shared/mod.rs` — `commit_max_file_size` config field.
- `src/tui/keys.rs` — dispatcher wiring and help text.
