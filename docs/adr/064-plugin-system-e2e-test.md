# ADR-064: Plugin system end-to-end integration test suite

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

The plugin system had unit tests for each component (manifest
validation, trust-tier filtering, env curation, sandbox policy, hook
verdict, verifier-bus registration) but no single end-to-end test that
exercises the full plugin lifecycle: load a plugin with all 4
capability kinds (skill + tool + hook + verifier), exercise each
through the executor, and verify the trust/sandbox/env-curation/audit
contracts hold.

A regression that breaks the *composition* (e.g. a plugin whose hook
runs before its verifier, or whose tool runs at the wrong trust tier)
would not be caught by the unit tests.

## Decision

Add a single end-to-end integration test
(`e2e_plugin_all_four_capability_kinds` in
`src/session/plugin_tools/tests.rs`) that:

1. Creates a temp plugin directory with a mock plugin manifest declaring
   all 4 capability kinds: a skill (`/e2e`), a tool (`e2e/echo`), a hook
   (`pre-tool-bash`), and a verifier (`e2e-check`).
2. Loads the plugin via `load_from_dir` with `TrustPolicy::up_to(Shell)`.
3. Asserts the skill is registered (`skill_by_trigger("/e2e")`) and
   renders its prompt (`skill_prompt("/e2e", "hello")`).
4. Asserts the tool is registered, callable via `PluginToolWrapper.run`,
   and produces the expected output.
5. Asserts the hook fires on `pre-tool-bash` with both verdicts: `Allow`
   (exit 0) and `Deny` (exit 2 via `KF_DENY=1`).
6. Asserts the verifier is registered (`verifier_by_name("e2e-check")`)
   and produces a `Fail` verdict via `PluginVerifier::run`.
7. Asserts trust filtering: at `ReadOnly` max with
   `reject_on_excess(false)`, the tool and hook are filtered out (they
   require `Shell`); the skill and verifier remain.
8. Asserts the audit log (WO 11.6) records the hook denial with the
   `e2e-plugin` name attribution.

The test is `#[cfg(unix)]` because it uses bash scripts for the
tool/hook/verifier commands. On Windows, shell scripts don't run; the
test is skipped.

## Consequences

- A regression that breaks the composition of the 4 capability kinds is
  now caught by a single test.
- The test does NOT replace the unit tests — it composes them. Each
  unit test still exists; the integration test proves they compose.
- The test is `#[cfg(unix)]` + uses `tempfile::tempdir()` for isolation;
  it runs in the default `cargo test` suite (no `#[ignore]` needed —
  it's fast, ~1s).
- The audit-log assertion (step 8) depends on WO 11.6 landing first.

## Notes

- The mock plugin scripts are executable (`chmod +x` in test setup).
- The test does NOT exercise the full executor turn loop — it exercises
  the plugin loader, tool wrapper, hook runner, and verifier directly.
  A full-executor integration test is a follow-up (the existing
  `plugin_verifier_triggers_correction_result` test covers the
  executor+verifier path).
- `ponytail:` / `ceiling:` annotations in the plugin code are
  preserved; the integration test is additive.