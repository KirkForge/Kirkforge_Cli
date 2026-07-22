# AGENTS.md — Worker Contract for KirkForge-Cli

*This file is the verifier contract for any AI agent working in this repo. Read it before starting. Follow it always. Violations are regressions.*

**See also**: [REPORULES.md](../REPORULES.md) — multi-machine sync, git identity, PAT handling, and new-repo bootstrap.
**See also**: [CLAUDE.md](CLAUDE.md) and [docs/adr/](docs/adr/) — ADRs that pin load-bearing decisions (don't break them silently).

## 0. Repo-specific guidance (existing — keep)

This repo is a Rust CLI coding agent (`kirkforge`). It uses `tokio`, `ratatui`, `crossterm`, `reqwest`, `serde`, `clap`, `tracing`, and `anyhow`. Conventions:

- Match the existing style: plain comments, `snake_case`, small pure helpers, `anyhow` for errors.
- Prefer `Edit` over full-file rewrites for small changes.
- Avoid adding dependencies unless necessary. The release profile is `opt-level = "z"` + `lto = true` + `codegen-units = 1` — binary size matters; a new dep must earn its place.
- The `kirkforge-testdoctor` crate is excluded from the workspace build (see `Cargo.toml`). Don't add it back without an ADR.
- The binary root lives at `src/main/mod.rs` (split form), not `src/main.rs`. The `[[bin]]` path in `Cargo.toml` is explicit — don't "fix" it.
- Run `scripts/ci-local.sh` (or `scripts/ci-local.sh quick`) before committing to reproduce the full CI matrix locally.

## 1. Plan mode default
- Before writing any code, write a plan to `workplan.md` (gitignored). The plan must list the files you will touch (full paths), state the root cause you're fixing (not the symptom), and state the gate you'll run to verify.
- Check `workplan.md` before implementation. Check `lessons.md` for lessons from prior sessions. Check `state.md` for current repo state.
- If the task is unclear, say so in `workplan.md` and escalate — do not guess.

## 2. Subagent strategy
- For complex multi-step tasks, break them into subtasks and dispatch subagents.
- Each subtask must have a clear scope (files to touch), a gate (command to run), and a done-condition.
- Do not dispatch a subagent for a task you can do in <5 minutes yourself.

## 3. Self-improving loop
- At session end, write `lessons.md` (gitignored) with: what you learned about this codebase (conventions, gotchas, patterns), what you tried that didn't work and why, what you'd do differently next time.
- Update `state.md` (tracked) with: what changed this session, what's pending, what's blocked.
- Lessons from `lessons.md` that are permanent conventions get folded into this `AGENTS.md` file — so the next worker reads them automatically.

## 4. Verification
- Run the gates before every commit. Paste the actual output (not paraphrased). A green claim requires the pasted output + the head SHA. "It passed" is not evidence.
- Gates for this repo:
  - Test: `cargo test --locked --workspace --no-fail-fast`
  - Lint: `cargo clippy --all-targets -- -D warnings`
  - Fmt: `cargo fmt --check`
  - Typecheck: `cargo check --workspace --all-targets`
- Integration tests (`scripts/run-integration-tests.sh`) need a live Ollama + `qwen2.5:0.5b`; they are NOT part of the default gate. Note if you ran them.
- Do not rewrite tests to make them pass. Fix the root cause.
- Do not add `|| true`, `|| echo "non-fatal"`, `#[ignore]` to make red go green.

## 5. Demand elegance
- Small, pure, well-named functions. No dead code. No debug spam (`println!`, `eprintln!`, `dbg!`, `tracing::debug!` left on in committed code) in committed code.
- Match the existing style: `snake_case`, `anyhow` for errors, plain comments (not doc-comments for internal helpers).
- Preserve honest-doc annotations — this repo uses `ponytail:` (pinned spec literals; if a `ponytail:` test fails, the spec and the impl drifted, not the test), `ceiling:`, and `upgrade path:`. They document known limitations and spec pins. Removing them is a regression. Editing a `ponytail:` literal without updating the corresponding ADR is a regression.
- A change that adds 100 lines to fix a 3-line bug is probably wrong. Find the smaller change.
- Avoid adding dependencies unless necessary (the release profile is size-optimized; every dep shows up in the binary).

## 6. Autonomous bug fixing
- If a test fails, read the error. Find the root cause. Fix it.
- Do NOT: rewrite the test to pass, add `|| true`, lower a threshold, delete the assertion, add `#[ignore]` to make red go green.
- Do NOT: add debug logging to committed code. Use `workplan.md` for scratch notes.
- If you've attempted the same fix 3 times and it's still red, STOP. Write "ESCALATE: <root cause unknown>" in `lessons.md` and return. The brain takes over when the brawn is stuck.

## Task management
1. **Plan**: write `workplan.md` (gitignored) with files to touch + root cause + gate.
2. **Check before implementation**: read `workplan.md`, `lessons.md`, `state.md`, and this `AGENTS.md`.
3. **Check progression**: after each file edit, verify it compiles/lints. Don't batch 10 changes then discover the 3rd was wrong.
4. **Explain changes**: post a summary in `workplan.md` (what changed, why) and a one-liner in `CHANGELOG.md` (it exists in this repo — keep the cadence).
5. **Commit after every task, not at the end.** Each task in the workorder is a gated commit. Commit it, push it, verify CI green, then move to the next task. Do NOT accumulate uncommitted work across tasks — if you do, you will lose it or break CI. At session close: write `lessons.md` (what I learned) → update `state.md` (what changed, what's pending) → `CHANGELOG.md` one-liner → verify `git status` shows clean tree (if it doesn't, you forgot to commit — commit now) → verify gates green → paste final gate output. Session is NOT done until `git status` is clean AND all gates are green.
6. **Worktree discipline**: work in an isolated worktree off `origin/dev` (this repo's default branch). `git fetch && git reset --hard origin/dev` before starting. Never touch `dev` directly. Never force-push. Fix forward.
7. **Scope discipline**: touch only the files the task names. If you need to edit outside scope, note it in `lessons.md` as "scope creep: <file> because <reason>".
8. **Honesty over claim**: paste gate output, never say "green" without the run ID + head SHA. An ADR that overclaims is a regression. A "CI green" citation for the wrong run ID is a regression.

## Escalation
If you are stuck after 3 attempts, say so. Write "ESCALATE: <root cause unknown>" in `lessons.md`. The brain (frontier model) takes over. This is not a failure — it's the design: the Fiat knows when to call the tow truck.