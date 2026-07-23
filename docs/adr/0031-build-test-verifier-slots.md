# ADR-0031: Build and Test Verifier Slots

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

The Rust runtime verifier bus already had `security` (priority 1), `lint` (priority 2), `git` (priority 3), and `rustfmt` (priority 4). This caught static issues after an edit but left two concrete correctness gaps in the ChatGPT-described verification pipeline:

1. **Did the change compile?** A model edit can introduce or leave a syntax, type, or borrow error that `lint`/`rustfmt` do not surface as a hard compilation failure.
2. **Did the targeted tests pass?** A change may break unit tests in the module it touches; the correction loop had no deterministic way to run just those tests and return the failure output to the model.

The correction loop at `src/session/verifier/correction.rs:41-133` already handles `Verdict::Fixable` with empty `original`/`replacement` and no `command`: it returns the `description` as a model-facing suggestion. Therefore new verifier slots can plug in without modifying the correction logic, as long as they emit `Fixable` with the compiler/test output in `description`.

## Decision

Add two new Rust-runtime verifier slots:

- **`build`** at priority 3 — runs `cargo build --message-format=json` in the detected Cargo root, parses `compiler-message` artifacts, and returns the first error for the edited file as `Verdict::Fixable(FixSuggestion { description: <message>, file, original: "", replacement: "", command: None, severity: "error" })`. Returns `Clean` when the build succeeds.
- **`test`** at priority 5 — derives a module path prefix from the edited Rust file (`src/foo/bar.rs` → `foo::bar`, `src/main.rs`/`src/lib.rs` → whole crate), runs `cargo test <prefix>` scoped to that module, and returns the failure output as `Verdict::Fixable` with the tail of stdout/stderr as the `description`. Returns `Clean` when the targeted tests pass.

The slot ordering is:

| Priority | Verifier | Why |
|----------|----------|-----|
| 1 | security | Block dangerous output before any other work |
| 2 | lint | Catch style/fixable warnings fast |
| 3 | build | Compilation is the next hard gate |
| 3 | git | Dirty-state checks remain at the same level |
| 4 | rustfmt | Format after correctness |
| 5 | test | Run only after code is formatted and compiles |

`build` and `git` share priority 3. The stable sort in `VerifierSlots::register` preserves insertion order, so `build` runs before `git` as registered in `init_default_verifiers`. This is acceptable because the two checks are independent.

### Suggestion, not auto-fix

Both slots return `Fixable` with empty `original`/`replacement` and `command: None`. The correction loop therefore treats them as **suggestions** and forwards the compiler/test output to the model verbatim. This is intentional:

- Compiler errors and test failures rarely have deterministic text replacements.
- Attempting a blind auto-fix from a compiler message risks changing the wrong code.
- The model already has the full context of the edit and is better placed to interpret the diagnostic.

## Consequences

**Positive:**
- The correction loop now catches "does not compile" and "tests fail" deterministically, closing the biggest verification gap versus the target pipeline.
- No new dependencies are required; both slots reuse existing `tokio::process`, `serde_json`, and the Cargo root detection from `lint.rs`.
- Scoped test execution (`cargo test <module-prefix>`) keeps the correction loop fast; we do not run the entire workspace test suite on every edit.
- The existing correction contract needs no changes.

**Negative:**
- `cargo build` and `cargo test` each take seconds on large crates, adding latency to the correction loop. They are run only for `.rs` file edits and only when a Cargo root is found.
- The module-path prefix heuristic can miss tests declared with `#[test]` outside the inferred module path (e.g. integration tests in `tests/` whose name does not match the file stem). This is a deliberate trade-off for speed and stability on stable Rust; future work can use `cargo test --no-run` + nightly JSON discovery for exact matching.
- Tests that spawn cargo are `#[ignore]` because the Cargo package-cache lock serializes cargo processes; they must be run separately with `cargo test --workspace -- --ignored --test-threads=1`.

## Implementation

- `src/session/verifier/build.rs` — new `verify_build` function and unit tests.
- `src/session/verifier/test.rs` — new `verify_test` function and unit tests.
- `src/session/verifier/mod.rs` — adds `pub mod build;` and `pub mod test;`.
- `src/session/executor/mod.rs` — registers `BuildV` (priority 3) and `TestV` (priority 5) in `init_default_verifiers`, and updates `BUILTIN_VERIFIERS` and the comment.
