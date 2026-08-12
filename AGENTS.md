# AGENTS.md — Worker Contract for KirkForge-Cli

*This file is the verifier contract for any AI agent working in this repo. Read it before starting. Follow it always. Violations are regressions.*

**See also**: [CLAUDE.md](CLAUDE.md) and [docs/adr/](docs/adr/) — ADRs that pin load-bearing decisions (don't break them silently).

## 0. Repo-specific guidance (existing — keep)

This repo is a Rust CLI coding agent (`kf-code`). It uses `tokio`, `ratatui`, `crossterm`, `reqwest`, `serde`, `clap`, `tracing`, and `anyhow`. Conventions:

- Match the existing style: plain comments, `snake_case`, small pure helpers, `anyhow` for errors.
- Prefer `Edit` over full-file rewrites for small changes.
- Avoid adding dependencies unless necessary. The release profile is `opt-level = "z"` + `lto = true` + `codegen-units = 1` — binary size matters; a new dep must earn its place.
- The `kf-testdoctor` crate is now a workspace member (WO 12.4). It provides `cargo run -p kf-testdoctor -- diagnose --root .` for self-diagnosis of test coverage gaps.
- The binary root lives at `src/main/mod.rs` (split form), not `src/main.rs`. The `[[bin]]` path in `Cargo.toml` is explicit — don't "fix" it.
- Run `scripts/ci-local.sh` (or `scripts/ci-local.sh quick`) before committing to reproduce the full CI matrix locally.

## 1. Plan mode default
- Before writing any code, write a plan to `workplan.md` (gitignored). The plan must list the files you will touch (full paths), state the root cause you're fixing (not the symptom), and state the gate you'll run to verify.
- Check `workplan.md` before implementation. Check `lessons.md` for lessons from prior sessions. Check `state.md` for current repo state.
- If the task is unclear, say so in `workplan.md` and escalate — do not guess.

## Phased workflow

**Fast-path exemption:** changes under 5 lines in a single file skip to Phase A + D + E only.

### Phase A — Comprehension (no edits)
- Read every file the task touches. Trace the flow end-to-end.
- Grep for cross-layer references (scripts, configs, docs).
- Gate: `workplan.md` exists with file list + root cause.

### Phase B — Impact analysis (mandatory for non-trivial changes)
- Run `gitnexus impact` on every symbol to be edited.
- Warn on HIGH/CRITICAL risk before proceeding.
- Gate: impact results summarized in `workplan.md`.

### Phase C — Verification
- Dispatch verification subagents for uncertain assumptions.
- Cross-layer grep before any rename/delete/API change:
  `grep -rn 'SYMBOL' src/ scripts/ .github/ docs/ *.toml crates/*/`
  Every reference must be updated in same commit or explicitly deferred.
- Gate: assumptions verified or marked "unverified — proceeding at risk."

### Phase D — Implementation
- Per-file edits with per-edit compile check.
- Commit after every task. Worktree discipline. Scope discipline.
- Gate: each edit compiles, each task is a gated commit.

### Phase E — Review + synthesis
- Run `detect_changes()`. Update `state.md`, `lessons.md`, `CHANGELOG.md`.
- Gate: clean tree, green gates, docs updated, deferred work disclosed.

## 2. Subagent strategy
Decision tree:
- **<5 min?** → do it yourself.
- **Read-only verification?** → dispatch explore subagents in parallel.
- **Multi-file write with independent files?** → dispatch general subagents in parallel.
- **Uncertain?** → verify first (Phase C), then implement (Phase D).
- **HIGH/CRITICAL risk?** → review subagent after implementation.
- Each subtask: clear scope (files), a gate (command), a done-condition.

## 3. Self-improving loop
- At session end, write `lessons.md` (gitignored) with: what you learned about this codebase (conventions, gotchas, patterns), what you tried that didn't work and why, what you'd do differently next time.
- Update `state.md` (tracked) with: what changed this session, what's pending, what's blocked.
- Lessons from `lessons.md` that are permanent conventions get folded into this `AGENTS.md` file — so the next worker reads them automatically.

## 4. Verification
- Run the gates before every commit. Paste the actual output (not paraphrased). A green claim requires the pasted output + the head SHA. "It passed" is not evidence.
- **Local gate (fast)** — run before every commit:
  - `scripts/test-fast.sh` (unit/lib/bins only, skips integration tests)
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --check`
  - `cargo check --workspace --all-targets`
- **Pre-merge gate (full)** — required before pushing to `dev`:
  - `scripts/test-full.sh` (all workspace tests)
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --check`
  - `cargo check --workspace --all-targets`
  - `cargo test -p kf-budget-core --test adr_xref_drift`
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

## 7. Codebase patterns
- The existing `Verifier` trait (`async fn verify(&self, event: &BusEvent) -> Verdict`) and the new `BusVerifier` trait (`fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry>`) coexist. The former is event-driven, the latter is sync and context-based. Don't try to unify them in one pass.
- `CorrectionResult` is a struct with `{verifier, success, message, fix}` fields — not an enum. There is no `CorrectionResult::Failed`.
- `tokio::task::block_in_place` panics in single-threaded test runtimes. When wrapping async code in sync adapters, use stubs or find another approach.
- `.map_or(true, |a| ...)` on `Option` triggers `clippy::unnecessary_map_or`. Use `.is_none_or(|a| ...)` instead (Rust 1.82+).
- When adding fields to `Config`, update ALL of: `Default` impl, struct definition, test `Config` literals (especially `executor/tests/mod.rs`), `adapter_for_with_provider` call sites, `adapter_for` convenience wrapper, and test calls.
- The `crates/kf-budget-core/README.md` `| Tests | N passing |` row counts `#[test]` attributes under `crates/` only, not the entire workspace. When adding tests to `crates/` sub-crates, bump the count.
- `bincode` is explicitly rejected project-wide (root `Cargo.toml` comment). Use `serde_json` for serialization.
- When adding serialization to a crate that already depends on `serde`, just add `serde_json` to the crate's `Cargo.toml` — don't introduce new serialization libraries.
- The `ContextIndex` struct has a private `symbols` field. When creating a cache format, use a separate struct (`CachedIndex`) that includes both the symbols and metadata (like git HEAD). Don't make the internal field public just for serialization.
- **ADR status is a two-source-of-truth system**: ADR file headers (`Status: ...`) AND the index table in `docs/adr/README.md` must agree. The `adr_xref_drift` test (`kf-budget-core`) will catch mismatches. When changing an ADR status, update BOTH the file header and the index table row. If you use a compound status like "Accepted (partially implemented)", it must appear identically in both places.
- **CI runs on push/PR** (`.github/workflows/ci.yml`) but is sometimes red — do not rely on it as the only gate. Run `scripts/ci-local.sh` before committing.
- **`headless_chrome::Tab` does NOT hold a strong ref to `Browser`**: The `Tab` handle is a weak reference. If you drop the `Browser`, the `Tab` becomes invalid. Always keep `Browser` alive alongside `Tab` — e.g., store both in an owning struct (`BrowserSessionOwner { _browser: Browser, tab: Tab }`).
- **Stale cleanup items are a real risk**: Before starting work on a "cleanup" or "missing feature" item from state.md or a workorder, grep the codebase first. Multiple items listed as "open" (persist plugin state, agent steps limit) turned out to be already shipped. Thirty seconds of `grep` saves an hour of duplicate work.
- **`lessons.md` is gitignored**: If you need it tracked, use `git add -f`. Otherwise, fold permanent lessons into `AGENTS.md` at session close and let `lessons.md` stay scratch-only.
- **`cargo clippy --all-targets` can be slow** (3-4 min on this repo). Budget time for full gate runs. Consider running just the failing test first to verify the fix, then run the full gate.

## Task management
1. **Plan**: write `workplan.md` (gitignored) with files to touch + root cause + gate.
2. **Check before implementation**: read `workplan.md`, `lessons.md`, `state.md`, and this `AGENTS.md`.
3. **Check progression**: after each file edit, verify it compiles/lints. Don't batch 10 changes then discover the 3rd was wrong.
4. **Explain changes**: post a summary in `workplan.md` (what changed, why) and a one-liner in `CHANGELOG.md` (it exists in this repo — keep the cadence).
5. **Commit after every task, not at the end.** Each task in the workorder is a gated commit. Commit it, push it, verify CI green, then move to the next task. Do NOT accumulate uncommitted work across tasks — if you do, you will lose it or break CI. At session close: write `lessons.md` (what I learned) → update `state.md` (what changed, what's pending) → `CHANGELOG.md` one-liner → verify `git status` shows clean tree (if it doesn't, you forgot to commit — commit now) → verify gates green → paste final gate output. Session is NOT done until `git status` is clean AND all gates are green.
6. **Worktree discipline**: work in an isolated worktree off `origin/dev` (this repo's default branch). `git fetch && git reset --hard origin/dev` before starting. Never touch `dev` directly. Never force-push. Fix forward.
7. **Scope discipline**: touch only the files the task names. If you need to edit outside scope, note it in `lessons.md` as "scope creep: <file> because <reason>".
8. **Honesty over claim**: paste gate output, never say "green" without the run ID + head SHA. An ADR that overclaims is a regression. A "CI green" citation for the wrong run ID is a regression.
9. **Doc-sync discipline**: if your change alters the architecture, the plugin system, the feature-flag set, the tool list, the hook system, the verifier bus, or the context index, you MUST update `docs/TECHNICAL.md` in the same commit. If your change completes or defers a workorder, update the workorder's `## Status` line in the same commit. If your change adds or removes an ADR, update the ADR count in `docs/TECHNICAL.md` and `state.md` in the same commit. Leaving docs stale after a code change is a regression — the assessment found ARCHITECTURE.md stale in the same session it was written because this rule was not enforced.
10. **Doc-placement rule**: new .md files go in the right directory on first write. Do not drop .md files in the repo root (exceptions: `README.md`, `CHANGELOG.md`, `CLAUDE.md`, `AGENTS.md`, `state.md`, `lessons.md`). When an idea becomes an ADR, move the idea file to `docs/archive/ideas/`. When a runbook or benchmark doc is superseded, move it to `docs/archive/`. The `docs/README.md` index is the source of truth for where things go — if you create a new directory under `docs/`, add it to the index.
10. **README is a landing page, not a tech manual**: the README stays short — quick start, why, links to docs. Technical detail lives in `docs/TECHNICAL.md`. Do not expand the README with architecture sections, manifest formats, or feature-flag tables.
11. **Defer-disclosure (no silent deferral)**: any work item NOT fully completed — kicked out, feature-gated off, stubbed, downscoped, or replaced with a smaller version than requested — MUST be explicitly disclosed. The disclosure MUST state: (a) **what** was deferred, (b) **why** (the concrete blocker/reason — not "later"), (c) the **exact remaining work** to finish it, and (d) **where it's tracked** (workorder ID / `state.md` "pending" / ADR). Silently disabling, gating off, or stubbing a requested feature to make a commit "pass" is a **REGRESSION**, not progress. Example — BAD: *"made kf-draw default-off"*. GOOD: *"made kf-draw default-off (DEFERRED: owner asked for full rust-native impl; deferred because [reason]; remaining: [X]; tracked in WO 21.x)"*. Deferrals go into `state.md` "pending" + a WO 21+ item, never into the void.

## Escalation
If you are stuck after 3 attempts, say so. Write "ESCALATE: <root cause unknown>" in `lessons.md`. The brain (frontier model) takes over. This is not a failure — it's the design: the Fiat knows when to call the tow truck.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **Kirkforge_Cli** (17665 symbols, 44827 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/Kirkforge_Cli/context` | Codebase overview, check index freshness |
| `gitnexus://repo/Kirkforge_Cli/clusters` | All functional areas |
| `gitnexus://repo/Kirkforge_Cli/processes` | All execution flows |
| `gitnexus://repo/Kirkforge_Cli/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
