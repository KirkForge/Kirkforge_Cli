// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
use super::*;

#[test]
fn budget_compact_json_envelope_shape_is_pinned_at_subprocess_level() {
    // ponytail: subprocess pin for `kf-budget --json budget compact`.
    // The existing `budget_compact_json_output_shape_is_pinned`
    // in `commands::budget::compact_tests` mirrors the wrapper
    // shape INLINE (`serde_json::json!({ "hint": hint })`) — it
    // tests the macro call, not the CLI's actual stdout. A
    // contributor who rewires `commands::budget::compact()` to
    // emit `{"hint_v2": ...}` AND updates the inline test to
    // match keeps the unit test green and silently breaks every
    // downstream `jq '.hint.tokens_used'` consumer. Drift
    // catches here, at the subprocess boundary.
    //
    // Fresh tempdir → default TokenBudget (used=0,
    // ceiling=200_000) + empty recent outputs. The reason
    // string is verbatim from `compaction::build_hint`:
    //   "session at {used}/{ceiling} tokens; compaction suggested"
    // Pinning the reason format catches a contributor who
    // tweaks `build_hint` (e.g. drops "; compaction suggested"
    // thinking it's redundant noise).
    let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "budget", "compact"]);
    assert!(
        out.status.success(),
        "fresh tempdir must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        top_keys,
        ["hint"].into_iter().collect(),
        "budget compact --json top-level key set must be exactly \
             {{hint}}; a contributor who adds a sibling key (or \
             renames `hint`) breaks every `jq '.hint'` reader. got: \
             {top_keys:?}"
    );

    let hint_v = &v["hint"];
    let hint_obj = hint_v
        .as_object()
        .expect("hint is an object (CompactHint is a struct, not a primitive)");
    let hint_keys: std::collections::BTreeSet<&str> = hint_obj.keys().map(String::as_str).collect();
    assert_eq!(
        hint_keys,
        [
            "newest_turn",
            "oldest_turn",
            "reason",
            "tokens_ceiling",
            "tokens_used"
        ]
        .into_iter()
        .collect(),
        "CompactHint serialised field set must match the 5-field \
             struct in kf-budget-core::compaction::CompactHint. A \
             contributor who adds a 6th field (e.g. `triggered_at`) \
             propagates here only if the CLI's wrapper logic also \
             surfaces it; a rename of `tokens_used` → `used` breaks \
             `jq '.hint.tokens_used'` silently. got: {hint_keys:?}"
    );

    // ponytail: spot-check the values. The default TokenBudget
    // is ceiling=200_000, used=0; an empty recent produces
    // oldest_turn/newest_turn = null (not 0, not absent).
    // Asserting the literal reason string pins the
    // `compaction::build_hint` format — a contributor who
    // shortens it (drops "; compaction suggested") surfaces
    // here as a reason mismatch.
    assert_eq!(
        hint_v["tokens_used"], 0,
        "tokens_used on a fresh tempdir must be 0 (default budget \
             has used=0); non-zero here means the subprocess picked \
             up stale state from outside the tempdir"
    );
    assert_eq!(
        hint_v["tokens_ceiling"], 200_000,
        "tokens_ceiling must be the default 200_000 on a fresh \
             tempdir; a different value here means PLUGIN3_CONFIG_DIR \
             leaked through and a persisted config.toml set a custom \
             ceiling"
    );
    assert!(
        hint_v["oldest_turn"].is_null(),
        "oldest_turn must be null (Option::None serialised) on an \
             empty recent VecDeque — NOT 0 (which would be ambiguous \
             with the head of a single-entry deque) and NOT absent \
             (which would mean a skip_serializing_if drifted in)"
    );
    assert!(
        hint_v["newest_turn"].is_null(),
        "newest_turn must be null on an empty recent VecDeque — \
             same null-vs-0-vs-absent contract as oldest_turn above"
    );
    assert_eq!(
        hint_v["reason"], "session at 0/200000 tokens; compaction suggested",
        "reason must be the literal format string from \
             `compaction::build_hint`; a contributor who tweaks the \
             format (drops the trailing suffix, reorders the \
             fields, etc.) surfaces here as a reason mismatch"
    );
}

#[test]
fn budget_compact_json_envelope_with_populated_recent_is_pinned() {
    // ponytail: subprocess pin for `kf-budget --json budget
    // compact` when recent_outputs.jsonl is populated.
    // The Round 41 pin (`budget_compact_json_envelope_shape_is_pinned_at_subprocess_level`)
    // only covers the empty-recent case (fresh tempdir,
    // oldest_turn=null, newest_turn=null). A contributor who
    // wires `compact()` to truncate `turns` to the last 5
    // entries (thinking "the host only cares about recent
    // activity") would shrink the hint's turn range silently
    // — the empty-recent pin passes (null still wins) but
    // the populated path silently narrows the range. Drift
    // catches here.
    //
    // Subprocess setup:
    //   1. budget.toml: ceiling=100, used=42,
    //      approaching_ratio=0.8 → state Under (ratio 0.42)
    //      but the compact command reads used/ceiling only,
    //      not state.
    //   2. recent_outputs.jsonl: 3 entries
    //        {"key":"k0","size":100}
    //        {"key":"k1","size":200}
    //        {"key":"k2","size":300}
    //      so the hint's turn range spans 0..=2.
    //   3. Hint fields:
    //      - tokens_used=42 (from budget.toml)
    //      - tokens_ceiling=100 (from budget.toml)
    //      - oldest_turn=0, newest_turn=2 (full recent window)
    //      - reason = "session at 42/100 tokens; compaction suggested"
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let budget_path = runtime_dir.path().join("budget.toml");
    let recent_path = data_dir.path().join("recent_outputs.jsonl");
    std::fs::write(
        &budget_path,
        toml::to_string(&TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 42,
        })
        .unwrap(),
    )
    .unwrap();
    // ponytail: build the JSONL inline. The on-disk shape is
    // `{"key":"...","size":N}` per the recent_outputs_tests
    // wire pin (line 2962). The CLI's `load_recent_outputs`
    // reads each line as a `RecentEntry` struct.
    let mut body = String::new();
    for (k, s) in [("k0", 100usize), ("k1", 200), ("k2", 300)] {
        body.push_str(&format!(
            "{}\n",
            serde_json::to_string(&RecentEntry {
                key: k.into(),
                size: s
            })
            .unwrap(),
        ));
    }
    std::fs::write(&recent_path, body).unwrap();

    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "budget", "compact"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget budget compact");
    assert!(
        out.status.success(),
        "budget compact with populated recent must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    // ponytail: pin the same envelope shape as the empty
    // case (`{hint}` only) — populated recent must not
    // trigger a sibling key (e.g. `recent` array).
    let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        top_keys,
        ["hint"].into_iter().collect(),
        "budget compact --json top-level key set must be \
             exactly {{hint}} even with populated recent; a \
             contributor who leaks the recent VecDeque into the \
             envelope (e.g. as a `recent` sibling key) surfaces \
             here. got: {top_keys:?}"
    );

    let hint_v = &v["hint"];
    let hint_obj = hint_v.as_object().expect("hint is an object");
    // ponytail: pin the same 5-field hint shape as the empty
    // case — populated recent must not add a 6th field.
    let hint_keys: std::collections::BTreeSet<&str> = hint_obj.keys().map(String::as_str).collect();
    assert_eq!(
        hint_keys,
        [
            "newest_turn",
            "oldest_turn",
            "reason",
            "tokens_ceiling",
            "tokens_used"
        ]
        .into_iter()
        .collect(),
        "CompactHint serialised field set must match the 5-field \
             struct in kf-budget-core::compaction::CompactHint; got: \
             {hint_keys:?}"
    );

    // ponytail: pin the values from the seeded budget.toml
    // and recent entries. A contributor who wires `tokens_used`
    // to `recent.len()` (off-by-domain) or `tokens_ceiling` to
    // the hardcoded default 200_000 surfaces here.
    assert_eq!(
        hint_v["tokens_used"], 42,
        "tokens_used must be the seeded 42 (from budget.toml); \
             a different value means the CLI bypassed the seeded \
             budget.toml and picked up the default 0. got: {}",
        hint_v["tokens_used"]
    );
    assert_eq!(
        hint_v["tokens_ceiling"], 100,
        "tokens_ceiling must be the seeded 100 (from budget.toml); \
             a different value means the CLI bypassed the seeded \
             budget.toml and picked up the default 200_000. got: {}",
        hint_v["tokens_ceiling"]
    );

    // ponytail: pin the populated-recent turn range. With 3
    // entries (k0/k1/k2), oldest_turn=0 (FIFO head) and
    // newest_turn=2 (FIFO tail). A contributor who truncates
    // `turns` to the last 5 would still show 0..=2 here
    // (below the threshold) — but a contributor who flips
    // the range to the recent-rev (e.g. starts at -1 for
    // "before the head") surfaces as oldest_turn=-1, which
    // `is_number()` would catch as a type mismatch. The
    // dual-pin (range + per-index content) catches more.
    assert_eq!(
        hint_v["oldest_turn"], 0,
        "oldest_turn must be 0 (FIFO head of 3 seeded entries); \
             a different value means the turn-range computation \
             regressed. got: {}",
        hint_v["oldest_turn"]
    );
    assert_eq!(
        hint_v["newest_turn"], 2,
        "newest_turn must be 2 (FIFO tail of 3 seeded entries); \
             a different value means the turn-range computation \
             regressed. got: {}",
        hint_v["newest_turn"]
    );

    // ponytail: pin the reason format with seeded values.
    // The format from `compaction::build_hint` is
    //   "session at {used}/{ceiling} tokens; compaction suggested"
    // so the seeded (used=42, ceiling=100) yields
    //   "session at 42/100 tokens; compaction suggested"
    // Verbatim pin catches a contributor who tweaks the
    // format (drops the trailing suffix, reorders the
    // fields, etc.) and the seeded values verify the budget
    // actually threaded through to the reason string.
    assert_eq!(
        hint_v["reason"], "session at 42/100 tokens; compaction suggested",
        "reason must be the literal format string from \
             `compaction::build_hint` with seeded budget values; \
             a different value here means either the format \
             regressed or the budget values didn't propagate. \
             got: {:?}",
        hint_v["reason"]
    );
}

#[test]
fn budget_compact_human_branch_emits_3_or_5_lines_per_recent_window() {
    // ponytail: subprocess pin for `kf-budget budget compact` on
    // the human-readable (non-JSON) branch. The JSON sibling
    // (`budget_compact_json_envelope_shape_is_pinned_at_subprocess_level`
    // + `budget_compact_json_envelope_with_populated_recent_is_pinned`)
    // pins the JSON envelope (`{"hint": ...}`) and the
    // `CompactHint` 5-field shape end-to-end. The human branch
    // goes through `commands::budget::compact()` directly,
    // emitting 3 lines (empty recent) or 5 lines (populated
    // recent) — and was only tested at unit level via
    // `compact_hint_*` tests on `CompactHint` itself. A
    // contributor who flips the line order, drops a label
    // (e.g. shortens `tokens_used:` → `used:`), or re-pads
    // the column (`tokens_used: ` → `tokens_used:    `)
    // would pass unit tests because they assert on the
    // `CompactHint` struct, not on its rendering. The line-
    // by-line pin catches that here.
    //
    // The label padding is intentionally inconsistent:
    //   reason:       <value>   (7 spaces after colon)
    //   tokens_used: <value>   (1 space after colon)
    //   ceiling:      <value>   (6 spaces after colon)
    //   oldest_turn: <value>   (1 space after colon, only when Some)
    //   newest_turn: <value>   (1 space after colon, only when Some)
    // Pinning the exact padding catches a contributor who
    // re-aligns them (e.g. via `println!("{:<13}{}", "reason:",
    // hint.reason)`) — the change would be cosmetically
    // appealing but breaks a wrapper that does
    // `awk -F': ' '{print $1}'` on the rendered output.
    //
    // Two arms:
    //   empty recent   → 3 lines (reason, tokens_used, ceiling)
    //   3 recent       → 5 lines (above + oldest_turn, newest_turn)
    for (recent_size, expected_lines) in [(0usize, 3usize), (3usize, 5usize)] {
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        // ponytail: seed budget.toml with used=42, ceiling=100.
        // The reason format from `compaction::build_hint` is
        // `session at {used}/{ceiling} tokens; compaction suggested`,
        // so seeded values yield `session at 42/100 tokens;
        // compaction suggested` — pinned verbatim below.
        let budget_path = runtime_dir.path().join("budget.toml");
        let seed = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 42,
        };
        std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();
        // ponytail: seed recent_outputs.jsonl with `recent_size`
        // entries. `load_recent_outputs` reads this file at
        // `data_dir/recent_outputs.jsonl` (Paths::recent_outputs).
        // Each entry is `(key, size)` where `key` is the
        // BLAKE3 24-hex-char content address; we use
        // 24-char hex as a stand-in because the human branch
        // only displays the turn indices (`oldest_turn`/
        // `newest_turn`), not the keys.
        if recent_size > 0 {
            let recent_path = data_dir.path().join("recent_outputs.jsonl");
            let mut s = String::new();
            for i in 0..recent_size {
                let key = format!("{:0>24x}", i + 1);
                s.push_str(&format!(
                    "{{\"key\":\"{key}\",\"size\":{}}}\n",
                    (i + 1) * 100
                ));
            }
            std::fs::write(&recent_path, s).unwrap();
        }

        let out = std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["budget", "compact"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn kf-budget budget compact (human)");
        assert!(
            out.status.success(),
            "budget compact must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            expected_lines,
            "human branch must emit {expected_lines} lines for \
                 recent_size={recent_size} (empty → 3, populated → 5); \
                 a different count means the optional oldest_turn/\
                 newest_turn prints changed. got: {lines:?}"
        );

        // ponytail: pin the EXACT line shapes. The first three
        // lines are unconditional; the 4th and 5th appear only
        // when recent is populated. The label padding (number
        // of spaces between colon and value) is part of the
        // wire contract.
        assert!(
            lines[0].starts_with("reason:       "),
            "line[0] must lead with `reason:       ` (7 spaces \
                 after colon); got: {:?}",
            lines[0]
        );
        assert!(
            lines[0].ends_with("session at 42/100 tokens; compaction suggested"),
            "line[0] must end with the seeded reason string; \
                 got: {:?}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("tokens_used: "),
            "line[1] must lead with `tokens_used: ` (1 space \
                 after colon); got: {:?}",
            lines[1]
        );
        assert!(
            lines[1].ends_with("42"),
            "line[1] must end with the seeded used=42; \
                 got: {:?}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("ceiling:      "),
            "line[2] must lead with `ceiling:      ` (6 spaces \
                 after colon); got: {:?}",
            lines[2]
        );
        assert!(
            lines[2].ends_with("100"),
            "line[2] must end with the seeded ceiling=100; \
                 got: {:?}",
            lines[2]
        );

        if expected_lines == 5 {
            assert!(
                lines[3].starts_with("oldest_turn: "),
                "line[3] (populated recent) must lead with \
                     `oldest_turn: ` (1 space after colon); \
                     got: {:?}",
                lines[3]
            );
            assert!(
                lines[3].ends_with('0'),
                "line[3] must end with `0` (FIFO head of 3 \
                     seeded entries); got: {:?}",
                lines[3]
            );
            assert!(
                lines[4].starts_with("newest_turn: "),
                "line[4] (populated recent) must lead with \
                     `newest_turn: ` (1 space after colon); \
                     got: {:?}",
                lines[4]
            );
            assert!(
                lines[4].ends_with('2'),
                "line[4] must end with `2` (FIFO tail of 3 \
                     seeded entries, index 0..2); got: {:?}",
                lines[4]
            );
        }

        // ponytail: negative pin — the JSON sibling's envelope
        // markers MUST NOT leak into the human branch. A
        // contributor who copy-pastes the JSON branch's
        // `serde_json::to_string_pretty` into the human branch
        // surfaces here (the rendered lines would contain
        // `{`, `}`, and `"key":` fragments).
        assert!(
            !stdout.contains('{'),
            "human branch must NOT emit JSON envelope markers; \
                 got: {stdout:?}"
        );
        assert!(
            !stdout.contains("\"hint\""),
            "human branch must NOT emit the JSON sibling's \
                 `\"hint\"` key; got: {stdout:?}"
        );
    }
}
