# ADR-0006: Tool output detection

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The PostToolUse hook fires for every tool result. Plugin3 must
decide whether to slice the result, slice it cheaply, or leave
it alone. The detector classifies the input.

The naive classifier — "if input > N bytes, slice" — wastes
work on short inputs that happen to be noisy and misses long
inputs that are dense with signal. A layered detector mirrors
Stratum ADR-0014 (magic bytes → structural → shape heuristics).

## Decision

### ToolOutputKind enum

```rust
// crates/plugin3-core/src/detector.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutputKind {
    /// Test runner output (cargo test, jest, pytest).
    TestRunner,
    /// Compiler output (rustc, gcc, tsc).
    Compiler,
    /// Build log (cmake, make, gradle).
    BuildLog,
    /// Generic command output (ls, cat, head).
    GenericShell,
    /// Search results (rg, grep, ag).
    SearchResults,
    /// File content (cat <file>).
    FileContent,
    /// JSON / structured data.
    Json,
    /// Unknown — apply default rules.
    Unknown,
}
```

### Layered detection

```rust
// crates/plugin3-core/src/detector.rs

pub fn detect(input: &str, tool_name: Option<&str>) -> ToolOutputKind {
    // Layer 1: tool name hint.
    if let Some(name) = tool_name {
        if let Some(kind) = from_tool_name(name) {
            return kind;
        }
    }
    // Layer 2: structural shape.
    if let Some(kind) = from_shape(input) {
        return kind;
    }
    ToolOutputKind::Unknown
}

fn from_tool_name(name: &str) -> Option<ToolOutputKind> {
    match name {
        "cargo test" | "jest" | "pytest" | "mocha" => Some(ToolOutputKind::TestRunner),
        "rustc" | "cargo build" | "tsc" | "gcc" => Some(ToolOutputKind::Compiler),
        "cmake" | "make" | "gradle" => Some(ToolOutputKind::BuildLog),
        "rg" | "grep" | "ag" => Some(ToolOutputKind::SearchResults),
        "cat" => Some(ToolOutputKind::FileContent),
        _ => None,
    }
}

fn from_shape(input: &str) -> Option<ToolOutputKind> {
    // ponytail: byte-slicing at 1024 panics on multi-byte UTF-8.
    // Floor to the nearest char boundary so CJK/emoji tool output
    // (think `cat` on a non-ASCII file) doesn't crash the
    // PostToolUse hook. The drift test
    // `from_shape_does_not_panic_on_utf8_boundary` pins the
    // behaviour for a 1024-ASCII + 2000-CJK input.
    let head_end = floor_char_boundary(input, 1024.min(input.len()));
    let head = &input[..head_end];
    if head.lines().any(|l| {
        l.starts_with("running ") || l.starts_with("test result:")
    }) {
        return Some(ToolOutputKind::TestRunner);
    }
    if head.contains("error[") || head.contains("warning:") {
        // rustc-style: error[E0001], warning: unused variable
        return Some(ToolOutputKind::Compiler);
    }
    let trimmed = head.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(ToolOutputKind::Json);
    }
    // ponytail: avoid materialising a `Vec<&str>` — short-circuit
    // on the first line over 200 bytes. Saves a per-detect-call
    // allocation. We track the line count manually because `.all()`
    // consumes the iterator.
    let mut lines = head.lines();
    let mut line_count: usize = 0;
    let mut all_short = true;
    for l in &mut lines {
        line_count += 1;
        if l.len() >= 200 { all_short = false; break; }
    }
    if line_count > 0 && all_short
        && head.matches(':').count() > line_count / 2
    {
        return Some(ToolOutputKind::SearchResults);
    }
    None
}
```

ponytail: the earlier draft specified a naive byte-slice
`&input[..input.len().min(1024)]` and a `.lines().all(...)`
SearchResults check. The MVP floors the head slice to the
nearest char boundary (CJK/emoji safety) and short-circuits
the SearchResults check on the first line ≥ 200 bytes (no
intermediate `Vec<&str>` allocation). Both refactors keep
the same classification behaviour — the test fixture at
`crates/plugin3-core/tests/fixtures/detector/` (and the
in-file tests `from_shape_*`) pins the per-kind output so
the refactor doesn't regress detection.

### Slicing rules per kind

```rust
pub fn should_slice(kind: ToolOutputKind, bytes: usize) -> Decision {
    let threshold = match kind {
        ToolOutputKind::TestRunner => 8 * 1024,       // cargo test output is verbose
        ToolOutputKind::BuildLog => 4 * 1024,
        ToolOutputKind::Compiler => 8 * 1024,         // rustc errors repeat
        ToolOutputKind::GenericShell => 2 * 1024,
        ToolOutputKind::SearchResults => 16 * 1024,
        ToolOutputKind::FileContent => usize::MAX,     // never auto-slice
        ToolOutputKind::Json => 4 * 1024,
        ToolOutputKind::Unknown => 8 * 1024,
    };
    if bytes >= threshold {
        Decision::Slice { keep_head: 4096, keep_tail: 4096 }
    } else {
        Decision::Keep
    }
}

pub enum Decision {
    Keep,
    Slice { keep_head: usize, keep_tail: usize },
}
```

### Why FileContent is excluded

A `cat <file>` result is by definition the file content.
Slicing it would force the user to retrieve the middle for any
operation that needs more than the head/tail. The user can
explicitly request slicing via the budget's auto-intervention
(ADR-0005), but the default is to pass file content through
unmodified.

### Detector caching

The detector result is cached per `(tool_name, head_hash)`
for the duration of a session, where `head_hash` is the
BLAKE3 hash of the first 1024 char-boundary bytes of the
input:

```rust
// crates/plugin3-core/src/orchestrator.rs

pub(crate) const DETECTOR_CACHE_CAP: usize = 64;

#[derive(Default)]
pub struct DetectorCache {
    entries: RefCell<std::collections::HashMap<(Option<String>, blake3::Hash), ToolOutputKind>>,
}

impl DetectorCache {
    pub fn new() -> Self { Self::default() }

    pub fn get_or_detect(&self, tool_name: Option<&str>, content: &str) -> ToolOutputKind {
        let mut entries = self.entries.borrow_mut();
        if entries.len() >= DETECTOR_CACHE_CAP {
            entries.clear();
        }
        // ponytail: BLAKE3 hashes the head (first 1024 char-boundary
        // bytes) — the shape detector reads only the head, so two
        // inputs that share a head share a detected kind. The earlier
        // `(tool_name, content.len())` key collided on two equally-
        // sized outputs with different shapes (e.g. an 8 KB cargo-
        // test body vs an 8 KB compiler body) and the second call
        // returned the cached kind of the first. The head hash
        // collapses the head to 32 bytes and distinguishes same-
        // length distinct-shape inputs.
        let head_end = floor_char_boundary(content, 1024.min(content.len()));
        let head_hash = blake3::hash(&content.as_bytes()[..head_end]);
        let key = (tool_name.map(str::to_owned), head_hash);
        entries.entry(key).or_insert_with(|| detector::detect(content, tool_name)).to_owned()
    }
}
```

ponytail: the earlier draft specified two things that the MVP
explicitly rejected: a `parking_lot::Mutex` wrapper, and a
`(tool_name, content.len())` key. The MVP uses
`std::cell::RefCell` (the cache is per-call single-threaded —
the orchestrator is `pub fn run(orch, outputs)` and not shared
across threads, ADR-0007) and the key is
`(Option<String>, blake3::Hash)` — a BLAKE3 hash of the head
(the first 1024 char-boundary bytes). The head hash distinguishes
same-length distinct-shape inputs (e.g. an 8 KB cargo-test body
vs an 8 KB compiler body) which the length-only key conflated.
The workspace does not depend on `parking_lot` (ADR-0017 §
Workspace Cargo.toml), and the 32-byte BLAKE3 head hash is
cheaper than the per-call `detect` work it skips on a hit.

The cache is bounded at 64 entries with clear-on-evict
semantics (a future LRU is a swap-in if measured hit-rates
show churn). A repeat detection on the same `(tool_name,
head_hash)` pair returns the cached kind in O(1). Two
distinct inputs of the same length produce distinct
`head_hash` values when their first 1024 bytes differ, so
they occupy distinct cache slots — the behavioural test
`detector_cache_distinguishes_same_length_different_shape`
(in `orchestrator.rs`) pins this directly: two equally-
sized inputs with different shapes grow the cache to two
entries (not one). A future contributor who simplifies the
key back to `(tool_name, content.len())` re-introduces the
same-length collision and surfaces here. 64 entries × ~32
bytes each ≈ 2 KB worst case — bounded, deterministic.

## Consequences

Negative first:

- Eight kinds is a lot. A contributor adding a new kind must
  edit the enum, the `from_tool_name` matcher, the
  `from_shape` matcher, and the slicing-rules table. The
  trade is the granularity lets the detector avoid slicing
  things it shouldn't.
- The detector is heuristic. A `cargo test` invocation that
  uses a custom reporter may not be detected as
  `TestRunner`. The fall-through is `Unknown` with a
  conservative threshold — safer than mis-detecting.

Positive:

- File content is honoured. Slicing a `cat` result would be
  user-hostile.
- The cache keeps the detector O(1) on hot paths.
- The layering (tool name → shape) matches Stratum ADR-0014;
  a contributor familiar with Stratum is familiar here.

## Implementation notes

The detector module is stateless. The cache lives in the
orchestrator module — `DetectorCache` in
`crates/plugin3-core/src/orchestrator.rs` — not in a
detector submodule (the cache is per-call
single-threaded and belongs with the orchestrator that owns
it, ADR-0007). A fresh session starts with an empty cache;
the cache fills as tools run.

The detector is invoked by the orchestrator (ADR-0007), not by
the hook directly. The hook hands the raw tool result to the
orchestrator; the orchestrator decides whether to slice.

A regression test (ADR-0016) pins the detector's output for a
known corpus of `(tool_name, input)` pairs. The test fixture
lives at `crates/plugin3-core/tests/fixtures/detector/`.