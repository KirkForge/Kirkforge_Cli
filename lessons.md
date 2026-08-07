# Lessons — WO 20 integration / session-death recovery

## Why the prior kf-code opencode session died (run `0fbedb9b`)
- It was running the final gate `cargo test --workspace --no-fail-fast` after the
  wo/20.7.0 merge. At 20:36:36 the **gitnexus MCP server dropped its connection**;
  opencode responded by `disposing all instances` (dir = `.../desktop`) and aborting
  the in-flight message. `error=Aborted stack=undefined`.
- Root cause = infra (MCP crash under memory pressure during the full workspace test),
  NOT a code edit. The repo was left clean and intact. No work was lost.
- Lesson: the **full workspace test gate is what OOMs/hangs** here. Don't run it in one
  shot. Verify per-module (`cargo test --lib -p kf-code <module>`) with `timeout` guards.
  This session re-ran the same gate shape and hit the same wall; per-module worked.

## Shared opencode.db confusion
- `~/.local/share/opencode/opencode.db` is shared across ALL projects (389 MB here).
  The log mixes sessions from KirkForge-Cli AND KirkForge-PicoSeries-picosentry.
- To find the RIGHT dead session, filter the log by `run=<id>` AND `cwd=`/path, not by
  the last line (concurrent runs interleave; the last line can be a different project).
- `run=f2b77884` was *this* session's own id (my own grep logged it) — don't mistake
  it for the corpse. The corpse was `run=0fbedb9b`.

## WO 20.2.0 merge — the load-bearing decisions
- **Old merge base (9d003b5) → stale "theirs".** wo/20.2.0 branched off the workorders-doc
  commit, before most other wo/20.x landed. So its versions of tests that integrate had
  since refined came in STALE. Symptom: the 4 `adapter_for_with_provider_selects_*` tests
  had their assertions *shuffled* (rotated by one) — compiled fine (clippy green!) but
  failed at runtime. **clippy green ≠ tests green.** Always run the touched module's tests.
- **Cache-breakpoint algorithm:** I first took integrate's `prefix_budget` variant; it
  marked the wrong message and failed 4 body-marking tests. wo/20.2.0's "count
  system+tools, then last-N user msgs" is simpler AND satisfies both the marking tests
  and the CRIT-1 cap-4 tests. Wrong call corrected after first test run. Lesson: let the
  test suite pick the algorithm when both are "valid" on paper.
- **`build_anthropic_body` arity:** combined signature is 9-arg. Resolved ~27 test
  call-sites with a paren-matching python script (7-arg → append `8192, None`; 8-arg →
  insert budget_tokens). Much faster than 27 hand-edits.
- **CONFIG_FIELD_COUNT drift guard** is the real canary: adding a ModelConfig field
  forces updates in 4 places (const, struct, Default, + the test's `merge_toml_source`
  TOML + MERGE_TOML_EXPECTED + ENV_OVERRIDE_EXPECTED + the ModelConfig=NN comment).

## Ponytail
- Don't run the full `cargo test --workspace` cold to "verify" — it's the exact
  resource hog that killed the last session. Per-module is faster and proves the
  merge-critical paths. Saved ~15 min × several avoided hangs.

## Git
- `git merge --no-commit` + resolve + `git add -A` + `git commit` DID produce a correct
  2-parent merge commit (verified `parents: <ours> <theirs>`). MERGE_HEAD survives `git add`.
- To list unmerged topic branches correctly: `git for-each-ref --merged <base> refs/heads/...`
  and `comm -23` against all. (Not `git merge-base --is-merged` + naive `git branch` parse —
  worktree `+` markers break it.)
