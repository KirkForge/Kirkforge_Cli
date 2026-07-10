# ADR-0016: Test strategy — parity, drift, property, golden

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

A plugin that fails silently is worse than a plugin that
fails loudly: the user's session produces subtly wrong output
and the user has no idea why. Plugin3's test strategy must:

- Catch regressions in the slicing orchestrator.
- Catch drift between Plugin3's `OffloadStore` and Stratum's.
- Catch drift between the canonical payload schema and the
  per-host shims.
- Catch property-level invariants (no panic, idempotence,
  byte-saved property).
- Run on every CI build in <5 minutes.

Mirrors Stratum ADR-0017. Plugin3's test pyramid is
structurally identical with one addition: a drift test
specifically for the byte-compat markers that share wire
format with Stratum.

## Decision

### Five test categories

1. **Unit tests** — per-function correctness.
2. **Property tests** — invariants across many inputs.
3. **Golden tests** — fixed input, expected output.
4. **Drift tests** — byte equality between canonical source
   and derived artefacts.
5. **Integration tests** — full pipeline run, end-to-end.

The pyramid: many unit tests at the base, fewer property
tests above them, fewer golden tests above those, one or two
drift tests at the top, and a single integration test at
the apex.

### Unit tests

Every public function in `plugin3-core` and `plugin3-cli` has
at least one unit test in the same file. The tests assert:

- The function's documented contract.
- Edge cases (empty input, very large input, malformed input).
- Error variants (per Stratum ADR-0011's three-variant enum).

```yaml
# CI coverage gate
- name: Coverage gate
  run: cargo llvm-cov --workspace --fail-under-lines 70
```

The 70% line-coverage gate is a floor, not a target.

### Property tests

`proptest` is **not** a workspace dependency — the MVP
uses inline LCG fixtures to keep the dep tree lean
(ADR-0017 § Workspace Cargo.toml). The key property
tests:

1. **No-panic property** — the LCG fixture (a `u64` LCG
   iterated to produce ASCII / CJK / emoji inputs of
   varying sizes) is fed to every public transform. The
   transform must return a `Result`; it must never panic.

   ```rust
   #[test]
   fn no_panic_on_any_input() {
       let c = LocalSummaryCompactor::default();
       for input in lcg_inputs() {
           let out = c.apply(&input).expect("no panic");
           assert!(out.bytes_saved <= input.len());
       }
   }
   ```

2. **Idempotence property** — running the slicer twice on
   the same input produces the same output structure (head
   + tail + marker). The middle's BLAKE3 key is deterministic.

3. **Bytes-saved property** — `bytes_saved <= bytes_in` for
   every slicing transform. A transform that *grows* the
   input is a bug.

4. **Marker validity property** — every
   `<<plugin3:slice:*>>` marker in the output is well-formed
   (24 hex chars, valid `validate_key` per ADR-0004).

5. **Budget state property** — `state()` returns Under when
   used is 0; Approaching when used >= ceiling * ratio;
   Over when used >= ceiling. The transitions are monotonic.

6. **Detector cache hit-rate property** — repeated calls to
   `detect` on the same `(tool_name, content_hash)` are O(1)
   after the first call. The cache is a measurable speedup.

The no-panic property is the load-bearing one.

### Golden tests

Golden tests live at `crates/plugin3-core/tests/golden/`:

```
crates/plugin3-core/tests/golden/
├── head_tail_slice/
│   ├── input.txt
│   ├── expected.txt
│   └── metadata.toml       # head_bytes, tail_bytes, expected bytes_saved
├── detector_classify/
│   ├── cargo_test.json
│   ├── expected.toml       # expected ToolOutputKind
│   └── ...
└── budget_state/
    ├── scenario.toml       # used, ceiling, ratio → expected state
    └── expected.toml
```

A golden test reads `input.*`, runs the pipeline, asserts
the output matches `expected.*` byte-for-byte. The first
time a golden test is added, the author generates the
expected output by running `BLESS=1 cargo test golden`; the
expected file is then committed and reviewed.

```rust
#[test]
fn golden_head_tail_slice() {
    let input = include_str!("golden/head_tail_slice/input.txt");
    let expected = include_str!("golden/head_tail_slice/expected.txt");
    let metadata: GoldenMetadata =
        toml::from_str(include_str!("golden/head_tail_slice/metadata.toml")).unwrap();
    let slicer = HeadTailSlicer {
        head_bytes: metadata.head_bytes,
        tail_bytes: metadata.tail_bytes,
    };
    let store = InMemoryOffloadStore::new();
    let result = slicer.apply(input, &store).unwrap();
    let actual = format!("{}{}{}",
        result.head,
        result.offload_marker.unwrap_or_default(),
        result.tail,
    );
    assert_eq!(actual, expected);
}
```

Golden tests fail loudly when a transform changes behaviour.

### Drift tests

Five drift tests:

1. **OffloadStore drift** — `plugin3-core`'s `make_key`
   produces the same output as Stratum's `make_offload_key`
   for a known corpus of inputs. The fixture lives at
   `crates/plugin3-core/tests/store_drift.rs` and reads
   from a vendored copy of Stratum's `make_offload_key`
   reference output.

2. **Canonical payload drift** — the canonical payload and
   response shapes in `plugin3-hosts` are pinned by inline
   tests in `plugin3-hosts/src/lib.rs` and by the CLI's
   `hooks/mod.rs` drift tests. When per-host shims graduate
   from stubs, the shim drift test will live in the shim module.

3. **Config drift** — the embedded `config.toml` parses to
   a `Plugin3Config` whose `to_string` matches the source
   file (after normalisation). Catches reformatting accidents.

4. **Usage record drift** — the `UsageRecord` JSON shape
   is pinned by a fixture. A contributor who renames a
   field fails CI.

5. **ADR cross-reference drift** — the README's "See also"
   links to ADRs that exist in the `docs/adr/` directory.
   A removed ADR with active links fails CI.

The OffloadStore drift test is the load-bearing one. It is
the single source of truth for "do Plugin3's slice markers
mean the same thing as Stratum's offload markers?".

### Integration tests

One integration test: `tests/cli_smoke.rs`. It runs:

```bash
echo '{"tool_name":"cargo test","tool_result_key":"abc","content":"..."}' \
    | cargo run --quiet --bin plugin3 -- hook post-tool-use
```

And asserts:

- Exit code 0.
- Stdout is a JSON object with `content` and `note` fields.
- The output's `content` is shorter than the input (slicing
  occurred).
- The output contains a `<<plugin3:slice:*>>` marker.

A second integration test exercises the budget guard:

```bash
echo '{"prompt":"explain the migration"}' \
    | PLUGIN3_BUDGET_CEILING=100 \
    cargo run --quiet --bin plugin3 -- hook user-prompt-submit
```

And asserts the response includes a `compact` variant
because the budget was exceeded.

The integration tests are slow (~10 s each) but run on every
CI build.

### Test isolation

Every test that touches the filesystem uses
`tempfile::tempdir()` and cleans up via `Drop`. No test
relies on `~/.config/plugin3/` existing. The CLI tests set
`PLUGIN3_CONFIG_DIR`, `PLUGIN3_DATA_DIR`, and
`PLUGIN3_RUNTIME_DIR` to tempdir paths before invoking the
binary.

### Coverage gate

CI runs `cargo llvm-cov --workspace --fail-under-lines 70`.
The 70% floor is below the achieved coverage; it exists to
catch the case where a new module is added without tests.

## Consequences

Negative first:

- Five categories of test is the same as Stratum; the
  contributor must pick the right one. The rule: unit first,
  property when invariants apply, golden when bytes matter,
  drift when derived artefacts exist, integration for
  end-to-end.
- The OffloadStore drift test references a vendored copy of
  Stratum's `make_offload_key` output. A breaking change in
  Stratum requires updating both the vendored reference and
  the test.

Positive:

- The no-panic property is structural: there is no code path
  in the orchestrator that can produce a panic from a
  transform error. The test enforces it.
- Golden tests catch regression in slicing behaviour.
- Drift tests catch silent divergence between Plugin3 and
  Stratum (OffloadStore) and between the canonical payload
  and the per-host shims.

## Implementation notes

The test files live alongside the source:

```
crates/plugin3-core/src/slicing.rs        # unit tests inline
crates/plugin3-core/tests/fixtures/        # input/expected pairs (TSV)

crates/plugin3-hosts/src/lib.rs            # Host enum + canonical payload drift tests
crates/plugin3-hosts/src/claude_code.rs    # stub module (future shim)

crates/plugin3-cli/src/hooks/mod.rs        # all three hook handlers
crates/plugin3-cli/tests/*_drift.rs        # per-ADR drift tests
```

ponytail: the earlier draft prescribed a separate
`tests/property.rs`, `tests/golden.rs`, and `tests/golden/`
directory plus `crates/plugin3-cli/tests/cli_smoke.rs` —
the MVP inlines property tests as `mod tests` blocks in each
source file (e.g. `slicing.rs::no_panic_on_any_input`,
`compaction.rs::no_panic_on_any_input` use an LCG rather
than `proptest` to avoid the dep), and uses TSV fixtures
under `tests/fixtures/` rather than `golden/`. The CLI's
end-to-end surface is exercised by the per-ADR drift tests
in `tests/cli_design_spec_drift.rs`, `hooks_mod_drift.rs`,
`compaction_spec_drift.rs`, etc. — no separate `cli_smoke.rs`.
The workspace does **not** depend on `proptest` or
`assert_cmd` (ADR-0017 § Workspace Cargo.toml); the LCG
fixture pattern is the cheapest deterministic alternative
(`proptest` adds ~150 KB for no measured coverage gain on
the deterministic no-panic property). The drift test
`adr_0016_test_files_match_adr_layout` (in
`tests/build_spec_drift.rs`) pins this layout.

The integration tests use `std::process::Command` for
shell-out assertions:

```rust
#[test]
fn cli_hook_post_tool_use_slices() {
    use std::process::{Command, Stdio};
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_plugin3"))
        .env("PLUGIN3_CONFIG_DIR", tmp.path())
        .env("PLUGIN3_DATA_DIR", tmp.path())
        .env("PLUGIN3_RUNTIME_DIR", tmp.path())
        .arg("hook").arg("post-tool-use")
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .spawn().unwrap();
    // ... pipe stdin, assert stdout contains the slice marker ...
}
```

The CI workflow runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo llvm-cov --workspace --fail-under-lines 70
```

The four steps are the load-bearing CI surface. A PR that
breaks any of them fails the build.