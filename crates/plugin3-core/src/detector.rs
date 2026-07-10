//! Tool output detector — classifies a (`tool_name`, content) pair so the
//! orchestrator can pick a per-kind slicing threshold.
//! Per ADR-0006. Layered: tool-name hint, then structural shape.

use crate::text::floor_char_boundary;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutputKind {
    TestRunner,
    Compiler,
    BuildLog,
    GenericShell,
    SearchResults,
    FileContent,
    Json,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Slice { keep_head: usize, keep_tail: usize },
}

/// Layered detect: tool-name hint → structural shape → Unknown.
#[must_use]
pub fn detect(input: &str, tool_name: Option<&str>) -> ToolOutputKind {
    if let Some(name) = tool_name {
        if let Some(kind) = from_tool_name(name) {
            return kind;
        }
    }
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
        "rg" | "grep" | "ag" | "Grep" => Some(ToolOutputKind::SearchResults),
        // ponytail: only literal "cat" matches; bash scripts that wrap cat
        // go through GenericShell. Add wrapped-cat when a real user reports
        // a wrong-default bug.
        //
        // Claude Code names: the `PostToolUse` event's `tool_name` is
        // the host-side tool id (`"Bash"`, `"Read"`, `"Edit"`,
        // `"Write"`, `"Glob"`, `"Grep"`, `"WebFetch"`, ...), not the
        // command run inside a Bash call. Without this arm, a 500 KB
        // `cat foo.txt` run via the `Read` tool falls through
        // `from_tool_name` → `from_shape` → `Unknown` → THRESHOLD_VERBOSE
        // and gets sliced, even though the user is reading a file
        // they almost certainly want whole. Map it to FileContent
        // (THRESHOLD_NEVER) so the host receives the full Read result.
        "cat" | "Read" | "Write" | "Edit" | "Glob" => Some(ToolOutputKind::FileContent),
        // Bash covers everything the shell runs. GenericShell is the
        // tightest threshold class (2 KB) which matches the user's
        // intent for ad-hoc commands.
        "Bash" => Some(ToolOutputKind::GenericShell),
        _ => None,
    }
}

fn from_shape(input: &str) -> Option<ToolOutputKind> {
    // ponytail: byte-slicing at 1024 panics on multi-byte UTF-8.
    // Floor to the nearest char boundary so CJK/emoji tool output
    // (think `cat` on a non-ASCII file) doesn't crash the post-tool-use hook.
    let head_end = floor_char_boundary(input, 1024.min(input.len()));
    let head = &input[..head_end];
    if head
        .lines()
        .any(|l| l.starts_with("running ") || l.starts_with("test result:"))
    {
        return Some(ToolOutputKind::TestRunner);
    }
    if head.contains("error[") || head.contains("warning:") {
        return Some(ToolOutputKind::Compiler);
    }
    let trimmed = head.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(ToolOutputKind::Json);
    }
    // ponytail: avoid materialising a `Vec<&str>` — short-circuit on
    // the first line over 200 bytes. Saves a per-detect-call
    // allocation. We track the line count manually because `.all()`
    // consumes the iterator.
    let mut lines = head.lines();
    let mut line_count: usize = 0;
    let mut all_short = true;
    for l in &mut lines {
        line_count += 1;
        if l.len() >= 200 {
            all_short = false;
            break;
        }
    }
    if line_count > 0 && all_short && head.matches(':').count() > line_count / 2 {
        return Some(ToolOutputKind::SearchResults);
    }
    None
}

/// Per-kind slicing threshold (ADR-0006). `FileContent` is excluded by
/// convention — slicing a `cat` result would force the user to retrieve
/// the middle for any operation that needs more than head/tail bytes.
#[must_use]
pub fn should_slice(kind: ToolOutputKind, bytes: usize) -> Decision {
    let threshold = match kind {
        // ponytail: merge identical thresholds so the table is read as
        // policy, not boilerplate. ADR-0006 owns the numbers.
        ToolOutputKind::TestRunner | ToolOutputKind::Compiler | ToolOutputKind::Unknown => {
            THRESHOLD_VERBOSE
        }
        ToolOutputKind::BuildLog | ToolOutputKind::Json => THRESHOLD_MEDIUM,
        ToolOutputKind::GenericShell => THRESHOLD_TIGHT,
        ToolOutputKind::SearchResults => THRESHOLD_LOOSE,
        ToolOutputKind::FileContent => THRESHOLD_NEVER,
    };
    if bytes >= threshold {
        Decision::Slice {
            keep_head: SLICE_HEAD_BYTES,
            keep_tail: SLICE_TAIL_BYTES,
        }
    } else {
        Decision::Keep
    }
}

// ponytail: per-kind thresholds (ADR-0006 § Slicing rules per kind).
// Extracted from the match above so drift tests can pin each number
// against the spec without duplicating the table. A contributor who
// tunes one threshold (e.g. SearchResults 16k → 8k) surfaces here
// without the boundary tests below also firing on the same change.
pub(crate) const THRESHOLD_VERBOSE: usize = 8 * 1024; // TestRunner, Compiler, Unknown
pub(crate) const THRESHOLD_MEDIUM: usize = 4 * 1024; // BuildLog, Json
pub(crate) const THRESHOLD_TIGHT: usize = 2 * 1024; // GenericShell
pub(crate) const THRESHOLD_LOOSE: usize = 16 * 1024; // SearchResults
pub(crate) const THRESHOLD_NEVER: usize = usize::MAX; // FileContent — pass-through

// ponytail: slice shape (ADR-0006 § Slicing rules per kind). A
// contributor who tunes 4096 → 2048 (halving the kept bytes) silently
// shrinks what the host gets back; both constants are load-bearing
// for downstream token-budget math.
pub(crate) const SLICE_HEAD_BYTES: usize = 4096;
pub(crate) const SLICE_TAIL_BYTES: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_hint_wins() {
        assert_eq!(
            detect("anything", Some("cargo test")),
            ToolOutputKind::TestRunner
        );
        assert_eq!(detect("anything", Some("cat")), ToolOutputKind::FileContent);
    }

    #[test]
    fn shape_layer_classifies_cargo_test_output() {
        let s = "running 5 tests\ntest foo ... ok\ntest bar ... FAILED\n";
        assert_eq!(detect(s, None), ToolOutputKind::TestRunner);
    }

    #[test]
    fn shape_layer_classifies_compiler_output() {
        let s = "error[E0001]: mismatched types\nwarning: unused variable\n";
        assert_eq!(detect(s, None), ToolOutputKind::Compiler);
    }

    #[test]
    fn shape_layer_classifies_json() {
        let s = r#"{"key": "value", "n": 1}"#;
        assert_eq!(detect(s, None), ToolOutputKind::Json);
    }

    #[test]
    fn unknown_falls_through() {
        assert_eq!(detect("hello world", None), ToolOutputKind::Unknown);
    }

    // ponytail: B1 fix — Claude Code's `PostToolUse` event sends the
    // host-side tool id (`"Read"`, `"Edit"`, `"Write"`, `"Bash"`,
    // `"Glob"`, `"Grep"`), NOT the CLI command name. A contributor
    // who sees "Read" fall through to Unknown would silently slice
    // every Read result over 8k — that's a wrong-default bug
    // (ADR-0006 § Wrong-default bugs). The mapping pins:
    //   Read/Write/Edit/Glob/cat → FileContent (NEVER auto-slice)
    //   Bash                    → GenericShell (tight 2k threshold)
    //   Grep                    → SearchResults (loose 16k threshold)
    // A contributor who, e.g., maps `Read` → Unknown (thinking "file
    // reads are like cat but only when small") surfaces here.
    #[test]
    fn from_tool_name_maps_claude_code_tool_names() {
        // FileContent class — Read/Write/Edit/Glob/cat must NEVER
        // auto-slice regardless of size. 1 MB sample guarantees
        // we'd hit THRESHOLD_VERBOSE if the mapping were wrong.
        let huge = "x".repeat(1024 * 1024);
        for name in ["Read", "Write", "Edit", "Glob", "cat"] {
            let kind = detect(&huge, Some(name));
            assert_eq!(
                kind,
                ToolOutputKind::FileContent,
                "{name:?} must classify as FileContent (never-slice); \
                 a wrong mapping slips large Read results into Unknown \
                 and gets them sliced at 8 KB"
            );
            assert_eq!(
                should_slice(kind, huge.len()),
                Decision::Keep,
                "{name:?} FileContent result must never auto-slice, even at 1 MB"
            );
        }

        // Bash routes to GenericShell (tightest 2k threshold).
        // A contributor who maps Bash → FileContent (thinking "Bash
        // looks like shell output, similar to cat") makes ad-hoc
        // bash invocations never slice, which is the wrong default
        // for testing/compiler output.
        let bash_out = "x".repeat(4 * 1024);
        let bash_kind = detect(&bash_out, Some("Bash"));
        assert_eq!(
            bash_kind,
            ToolOutputKind::GenericShell,
            "Bash must classify as GenericShell (2k threshold); \
             FileContent here would silently never slice test output"
        );
        // 4 KB > THRESHOLD_TIGHT (2 KB) → must Slice.
        let bash_d = should_slice(bash_kind, bash_out.len());
        assert!(
            matches!(bash_d, Decision::Slice { .. }),
            "4 KB Bash output must Slice at the GenericShell 2 KB threshold"
        );

        // Grep routes to SearchResults (16k threshold).
        let grep_kind = detect(&"x".repeat(8 * 1024), Some("Grep"));
        assert_eq!(
            grep_kind,
            ToolOutputKind::SearchResults,
            "Grep must classify as SearchResults (16k threshold)"
        );

        // Sanity: a tool name that's neither CLI nor Claude Code
        // still falls through to shape detection.
        let plain = detect("hello world", Some("MysteryTool"));
        assert_eq!(
            plain,
            ToolOutputKind::Unknown,
            "non-mapped tool names must still fall through to shape detection"
        );
    }

    // ponytail: shape layer's SearchResults heuristic (lines 70-81)
    // is the only detector branch that is NOT pinned by a body-only
    // test — `tool_name_hint_wins` covers `rg|grep|ag` via the
    // tool-name fast path, and `search_results_slice_at_16k` covers
    // the threshold, but the actual shape classification
    // (`:` density > line_count/2 AND all lines < 200 bytes AND at
    // least one line) is load-bearing for any host that doesn't
    // tag outputs with a tool_name (Cursor, Aider shells that
    // strip the command name). Without this test a contributor who
    // flips `>` to `>=` on the colon-count silently changes the
    // heuristic on a single-colon line; one who drops the
    // `all_short` clause makes long-line search results fall
    // through to Unknown.
    #[test]
    fn shape_layer_classifies_search_results() {
        // Real rg output: <path>:<line>:<col>:<text>. Each line is
        // short and has at least two colons (well above the
        // line_count/2 threshold).
        let rg = "src/foo.rs:42:5:let x = 1;\n\
                  src/bar.rs:13:9:pub fn foo() {}\n\
                  src/baz.rs:99:1:// comment\n";
        assert_eq!(
            detect(rg, None),
            ToolOutputKind::SearchResults,
            "rg-shaped output (short lines + dense colons) must classify as SearchResults"
        );

        // The exact-density edge: 2 lines, 1 colon — `count > line_count/2`
        // is `1 > 1` = false, so this is NOT SearchResults. Pin the
        // strict `>` so a flip to `>=` surfaces.
        let exact = "a:b\nc\n";
        assert_ne!(
            detect(exact, None),
            ToolOutputKind::SearchResults,
            "1 colon across 2 lines (count == line_count/2) must NOT classify \
             as SearchResults; the heuristic uses `>` not `>=`"
        );

        // The density floor: 2 colons across 2 lines (count > line_count/2
        // is `2 > 1` = true) AND short → SearchResults.
        let dense = "a:b:c\nd:e:f\n";
        assert_eq!(
            detect(dense, None),
            ToolOutputKind::SearchResults,
            "2+ colons per line with all-short lines must classify as SearchResults"
        );

        // The all-short guard: a line over 200 bytes disqualifies the
        // whole head. A contributor who drops `all_short` would
        // accept minified-JSON-as-one-line as SearchResults (which
        // would then skip the Json branch's earlier return).
        let long_line = format!("a:b\n{}", "x".repeat(300));
        assert_ne!(
            detect(&long_line, None),
            ToolOutputKind::SearchResults,
            "a single line >= 200 bytes must disqualify SearchResults even \
             if colon density would otherwise pass"
        );

        // Empty input — line_count == 0, heuristic returns None.
        let empty = "";
        assert_eq!(detect(empty, None), ToolOutputKind::Unknown);
    }

    #[test]
    fn file_content_never_sliced() {
        // 1 MB cat result — still Keep.
        let d = should_slice(ToolOutputKind::FileContent, 1024 * 1024);
        assert_eq!(d, Decision::Keep);
    }

    #[test]
    fn unknown_slices_at_8k() {
        assert_eq!(
            should_slice(ToolOutputKind::Unknown, 4 * 1024),
            Decision::Keep
        );
        let Decision::Slice { .. } = should_slice(ToolOutputKind::Unknown, 8 * 1024) else {
            panic!("expected Slice at 8KB");
        };
    }

    #[test]
    fn search_results_slice_at_16k() {
        assert_eq!(
            should_slice(ToolOutputKind::SearchResults, 8 * 1024),
            Decision::Keep
        );
        let Decision::Slice { .. } = should_slice(ToolOutputKind::SearchResults, 16 * 1024) else {
            panic!("expected Slice at 16KB");
        };
    }

    #[test]
    fn from_shape_does_not_panic_on_utf8_boundary() {
        // 1024 ASCII + 2000 CJK (3 bytes each) = 7024 bytes. The 1024th
        // byte is an ASCII char, but the slice still needs to terminate
        // on a char boundary when input is >= 1024 bytes with multi-byte
        // content extending past the slice.
        let mut input = "a".repeat(1024);
        input.push_str(&"你".repeat(2000));
        // Before the floor_char_boundary fix this panicked at the
        // `&input[..1024]` byte slice.
        let _ = detect(&input, Some("cat"));
    }

    #[test]
    fn from_shape_works_on_pure_cjk_oversized_input() {
        // All 3-byte chars. 2000 chars = 6000 bytes; byte 1024 lands
        // inside a codepoint. Must not panic.
        let input = "你".repeat(2000);
        let kind = detect(&input, None);
        assert_eq!(kind, ToolOutputKind::Unknown);
    }

    // ---- ADR-0006 § Slicing rules per kind: threshold table. ----

    // ponytail: pin each threshold constant by value. A contributor
    // who tunes one (e.g. SearchResults 16k → 8k) without updating
    // the spec surfaces here AND in the boundary tests below.
    #[test]
    fn threshold_constants_are_pinned() {
        assert_eq!(THRESHOLD_VERBOSE, 8 * 1024);
        assert_eq!(THRESHOLD_MEDIUM, 4 * 1024);
        assert_eq!(THRESHOLD_TIGHT, 2 * 1024);
        assert_eq!(THRESHOLD_LOOSE, 16 * 1024);
        assert_eq!(THRESHOLD_NEVER, usize::MAX);
    }

    // ponytail: pin the slice head/tail constants by value. ADR-0006
    // § Slicing rules says "Slice { keep_head: 4096, keep_tail: 4096 }"
    // — changing the shape changes every orchestrator decision the
    // host consumes.
    #[test]
    fn slice_shape_constants_are_pinned() {
        assert_eq!(SLICE_HEAD_BYTES, 4096);
        assert_eq!(SLICE_TAIL_BYTES, 4096);
    }

    // ponytail: end-to-end threshold table. We assert Keep below
    // threshold, Slice AT-or-above (the spec says `bytes >= threshold`
    // — pin the >= to catch a contributor who flips it to >).
    // Hard-coded thresholds (8K, 4K, 16K, 2K) so the test catches a
    // constant change rather than self-referencing THRESHOLD_*.
    #[test]
    fn threshold_table_end_to_end_at_or_above_boundary() {
        // (kind, threshold_used_in_table, one_below, exactly_at, one_above)
        let rows: &[(ToolOutputKind, usize, usize, usize, usize)] = &[
            (
                ToolOutputKind::TestRunner,
                8 * 1024,
                8 * 1024 - 1,
                8 * 1024,
                8 * 1024 + 1,
            ),
            (
                ToolOutputKind::Compiler,
                8 * 1024,
                8 * 1024 - 1,
                8 * 1024,
                8 * 1024 + 1,
            ),
            (
                ToolOutputKind::Unknown,
                8 * 1024,
                8 * 1024 - 1,
                8 * 1024,
                8 * 1024 + 1,
            ),
            (
                ToolOutputKind::BuildLog,
                4 * 1024,
                4 * 1024 - 1,
                4 * 1024,
                4 * 1024 + 1,
            ),
            (
                ToolOutputKind::Json,
                4 * 1024,
                4 * 1024 - 1,
                4 * 1024,
                4 * 1024 + 1,
            ),
            (
                ToolOutputKind::GenericShell,
                2 * 1024,
                2 * 1024 - 1,
                2 * 1024,
                2 * 1024 + 1,
            ),
            (
                ToolOutputKind::SearchResults,
                16 * 1024,
                16 * 1024 - 1,
                16 * 1024,
                16 * 1024 + 1,
            ),
        ];
        for (kind, _, below, at, above) in rows {
            // ponytail: at-threshold must Slice (the spec is `>=`).
            // A `>` regression here surfaces immediately.
            let at_d = should_slice(*kind, *at);
            assert!(
                matches!(at_d, Decision::Slice { .. }),
                "{kind:?} at exactly {at} must Slice (>= boundary); got {at_d:?}"
            );
            // And below/above behave predictably.
            assert_eq!(
                should_slice(*kind, *below),
                Decision::Keep,
                "{kind:?} below threshold must Keep"
            );
            let above_d = should_slice(*kind, *above);
            assert!(
                matches!(above_d, Decision::Slice { .. }),
                "{kind:?} above threshold must Slice"
            );
            // Slice decision carries the spec'd head/tail.
            if let Decision::Slice {
                keep_head,
                keep_tail,
            } = above_d
            {
                assert_eq!(
                    keep_head, SLICE_HEAD_BYTES,
                    "{kind:?} slice head must equal ADR-0006's 4096"
                );
                assert_eq!(
                    keep_tail, SLICE_TAIL_BYTES,
                    "{kind:?} slice tail must equal ADR-0006's 4096"
                );
            }
        }
        // FileContent is the never-slice exception. We pick 16 GiB
        // — finite but bigger than any realistic output — to avoid
        // `usize::MAX - 1` math (`bytes >= usize::MAX` is always
        // true). A contributor who narrows the threshold (e.g.
        // FileContent = 1 MiB) surfaces here.
        let huge: usize = 16 * 1024 * 1024 * 1024;
        assert_eq!(
            should_slice(ToolOutputKind::FileContent, huge),
            Decision::Keep,
            "FileContent must never auto-slice"
        );
    }
}
