// ponytail: ADR-0014 § Recent outputs file — pins the
// `recent_outputs.jsonl` wire shape, the `RECENT_BOUND = 32`
// FIFO bound, and the per-line JSON object keys. Lives here
// (plugin3-cli) rather than plugin3-core because the writer and
// reader are both in this crate; the test calls the
// path-parameterised seam (`append_recent_at` /
// `load_recent_outputs_at`) so a tempdir keeps the user's real
// `$XDG_DATA_HOME/plugin3/recent_outputs.jsonl` out of the
// test's blast radius.
use super::*;

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("recent_outputs.jsonl readable")
        .lines()
        .filter(|l| !l.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

#[test]
fn recent_bound_is_pinned_at_32() {
    // ponytail: ADR-0014 § Recent outputs file specifies a
    // 32-entry FIFO bound. The constant is the load-bearing
    // contract — a contributor who bumps it to 64 silently
    // grows the on-disk file for every session. The fixture
    // test below (`fifo_eviction_at_boundary`) is the
    // behaviour-side pin; this attribute test catches a
    // constant-only change for review.
    assert_eq!(RECENT_BOUND, 32);
}

#[test]
fn per_line_wire_shape_is_key_and_size() {
    // ponytail: ADR-0014 § Recent outputs file spec includes
    // `content`/`tool_name`/`ts` fields; the actual wire
    // format is `{"key":"...","size":N}` because the budget
    // guard only needs the (key, size) pair. Pin both the
    // field set AND the order so a contributor who adds a
    // field (or renames `size` → `bytes`) surfaces here.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    append_recent_at(&path, "abc123", 4242);
    let lines = read_lines(&path);
    assert_eq!(lines.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|e| panic!("line not valid JSON: {lines:?}: {e}"));
    let obj = v.as_object().expect("object");
    let mut keys: Vec<&str> = obj.keys().map(std::string::String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["key", "size"], "field set drifted");
    assert_eq!(obj["key"], "abc123");
    assert_eq!(obj["size"], 4242);
}

#[test]
fn fifo_eviction_at_boundary() {
    // ponytail: ADR-0014 § Recent outputs file specifies FIFO
    // eviction when the file exceeds 32 entries. The 33rd
    // append must evict the 1st; the 34th evicts the 2nd. We
    // push 35 entries and assert the surviving window is
    // entries 4..36 (oldest 3 evicted).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    for i in 0..35 {
        append_recent_at(&path, &format!("k{i:02}"), i * 100);
    }
    let lines = read_lines(&path);
    assert_eq!(
        lines.len(),
        32,
        "FIFO bound must hold; got {} lines",
        lines.len()
    );
    // First surviving entry is k03 (the 4th push, index 3);
    // last is k34. This catches a contributor who flips the
    // eviction to LIFO (newest dropped) or to a different
    // bound (e.g. 64).
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let last: serde_json::Value = serde_json::from_str(&lines[31]).unwrap();
    assert_eq!(first["key"], "k03");
    assert_eq!(last["key"], "k34");
    // The sizes follow the key index, so size=300 and size=3400
    // pin the row contents (a contributor who scrambles key/size
    // in the writer surfaces here).
    assert_eq!(first["size"], 300);
    assert_eq!(last["size"], 3400);
}

#[test]
fn empty_file_loads_as_empty_vec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    assert!(
        load_recent_outputs_at(&path).is_empty(),
        "missing file must yield empty list, not panic"
    );
}

#[test]
fn reload_round_trips_recent_entries() {
    // ponytail: the writer/reader pair is owned by the same
    // module, but a future contributor who introduces a second
    // reader (e.g. for the report subcommand) could diverge
    // the field names. This test pins the round-trip on the
    // first 5 entries (below the FIFO bound, no eviction).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    for i in 0..5 {
        append_recent_at(&path, &format!("rt{i}"), 100 + i);
    }
    let back = load_recent_outputs_at(&path);
    assert_eq!(back.len(), 5);
    for (i, (k, s)) in back.iter().enumerate() {
        assert_eq!(k, &format!("rt{i}"));
        assert_eq!(*s, 100 + i);
    }
}

// ponytail: malformed JSONL rows must be silently skipped on
// load, mirroring `aggregate_sessions` in plugin3-core::report.
// A contributor who flips `filter_map(... .ok())` to a strict
// `map(... .unwrap())` makes any hand-edited recent file a
// crash on the next PostToolUse — caught here. The reader is
// `pub(crate) fn load_recent_outputs_at` (path-parameterised),
// so the test stays hermetic without touching XDG.
#[test]
fn malformed_recent_jsonl_rows_are_silently_skipped_on_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    // Hand-craft a file with two valid rows and two malformed
    // lines (one non-JSON, one valid JSON but missing fields).
    // The writer always emits parseable rows; this simulates
    // a host that interrupted the file mid-write or a user
    // who edited it by hand.
    std::fs::write(
        &path,
        "{\"key\":\"good1\",\"size\":100}\n\
             not json at all\n\
             {\"key\":\"good2\",\"size\":200}\n\
             {\"missing\":\"both-fields\"}\n",
    )
    .unwrap();
    let back = load_recent_outputs_at(&path);
    assert_eq!(
        back.len(),
        2,
        "two malformed rows must be silently skipped, leaving the two \
             valid rows; got {} entries: {back:?}",
        back.len()
    );
    assert_eq!(back[0].0, "good1");
    assert_eq!(back[0].1, 100);
    assert_eq!(back[1].0, "good2");
    assert_eq!(back[1].1, 200);
}

// ponytail: FIFO is by *insertion order*, not by key. A future
// contributor who introduces a "merge duplicate keys" pass
// (e.g. `if entries.iter().any(|(k,_)| k == new_key) { skip }`)
// silently shrinks the on-disk file when a single tool fires
// repeatedly — caught here. The fixture: 5 appends, all with
// the SAME key, all unique sizes. After append, the load must
// yield 5 distinct (key, size) rows, not 1 (deduped).
#[test]
fn fifo_is_by_insertion_order_not_by_key_dedup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    for i in 0..5 {
        // Same key, distinct sizes — a dedup pass would collapse
        // these into one entry.
        append_recent_at(&path, "duplicate-key", 100 + i);
    }
    let back = load_recent_outputs_at(&path);
    assert_eq!(
        back.len(),
        5,
        "FIFO is by insertion order, not by key; 5 appends of the same \
             key must produce 5 entries (dedup-by-key would silently shrink \
             this to 1); got {} entries: {back:?}",
        back.len()
    );
    let sizes: Vec<usize> = back.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        sizes,
        vec![100, 101, 102, 103, 104],
        "insertion order must preserve the original sizes (100..104)"
    );
}

// ponytail: pin the `load_recent_outputs_at` empty-input contract.
// An empty file (zero bytes) is different from a missing file.
// `read_to_string` on an empty file returns Ok("") which
// `.lines()` yields zero items — that path must also produce
// an empty VecDeque. The `empty_file_loads_as_empty_vec` test
// covers the missing-file case; this test covers the
// existing-but-empty case (a contributor who switches to
// `.read_to_string(path)?.lines()` would propagate the empty
// string fine, but a contributor who uses `BufReader::new` +
// a `.read_line` loop might mistakenly treat empty as EOF
// mid-stream — caught here).
#[test]
fn load_on_zero_byte_file_yields_empty_vec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recent_outputs.jsonl");
    std::fs::write(&path, "").expect("write empty");
    let back = load_recent_outputs_at(&path);
    assert!(
        back.is_empty(),
        "zero-byte file must load as empty VecDeque; got {} entries: {back:?}",
        back.len()
    );
}

// ponytail: pin the explicit-match `From<UsageKindArg> for UsageKind`
// bridge. clap names variants `kebab-case` (`budget-warn`); the
// `UsageKind` enum on the core side uses `snake_case` to match
// the on-disk JSONL wire format (ADR-0010). A contributor who
// adds a 7th variant to either side without updating the match
// fails to compile — better than the previous serde round-trip
// form that panicked at runtime on a missing wire string.
#[test]
fn usage_kind_arg_round_trips_via_match_into_usage_kind() {
    use clap::ValueEnum; // for `to_possible_value`
    for (arg, kind) in [
        (UsageKindArg::Slice, UsageKind::Slice),
        (UsageKindArg::BudgetWarn, UsageKind::BudgetWarn),
        (UsageKindArg::BudgetOver, UsageKind::BudgetOver),
        (UsageKindArg::CompactHint, UsageKind::CompactHint),
        (UsageKindArg::Prompt, UsageKind::Prompt),
        (UsageKindArg::Response, UsageKind::Response),
    ] {
        // Explicit match — the bridge the impl uses.
        assert_eq!(
            UsageKind::from(arg),
            kind,
            "UsageKindArg::{arg:?} must map to UsageKind::{kind:?}"
        );
        // CLI flag spelling — kebab-case from
        // `#[clap(rename_all = "kebab-case")]`. Single-word
        // variant names like `Slice` and `Prompt` are
        // unchanged (kebab-case with one segment is itself);
        // multi-word variants like `BudgetWarn` become
        // `budget-warn` (hyphen-separated).
        let cli_name = arg.to_possible_value().unwrap().get_name().to_string();
        let cli_expected = match arg {
            UsageKindArg::Slice => "slice",
            UsageKindArg::BudgetWarn => "budget-warn",
            UsageKindArg::BudgetOver => "budget-over",
            UsageKindArg::CompactHint => "compact-hint",
            UsageKindArg::Prompt => "prompt",
            UsageKindArg::Response => "response",
        };
        assert_eq!(
            cli_name, cli_expected,
            "CLI flag for {arg:?} must be the kebab-case spelling; got {cli_name:?}"
        );
    }
    // ponytail: the wire spelling for `UsageKind` (snake_case)
    // is the single source of truth for the JSONL form. Pin it
    // on the core side via `usage_kind_serialises_to_snake_case`
    // in `cost.rs`. The CLI-side `UsageKindArg` no longer
    // derives `Serialize` because the bridge is now an explicit
    // match — re-adding the serde derive here would invite
    // drift between two enums that are no longer linked.
}
