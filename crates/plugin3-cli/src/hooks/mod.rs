//! Hook handlers — `PostToolUse`, `UserPromptSubmit`, `PreCompact`.
//! Per ADR-0002 § Crate layout (`crates/plugin3-cli/src/hooks/`).
//!
//! ponytail: this split is the first of several prescribed by
//! ADR-0002. The hook handlers are the highest-leverage split
//! because they own the host-facing JSON contract; budget/report/
//! config live in main.rs until a future contributor moves them
//! into `commands/`.

use serde::{Deserialize, Serialize};
use serde_json::json;

use plugin3_core::{
    budget::{decide, estimate_tokens, Intervention},
    compaction::{self, CompactionTransform, LocalSummaryCompactor, Turn},
    cost::{classify_kind, emit_usage, UsageKind, UsageRecord},
    run_orchestrator,
    slicing::HeadTailSlicer,
    DetectorCache, SliceDecision, SlicingOrchestrator,
};
use plugin3_hosts::{
    detect_host, Host, PostToolUsePayload, PostToolUseResponse, UserPromptSubmitPayload,
};

use super::{append_recent, emit_compact_hint, empty_record, open_store, read_stdin_json};

/// Serialise `value` to one line of JSON and print it. If serialisation
/// somehow fails (it shouldn't for our derive-Serialize shapes), log the
/// error to stderr and print `fallback` so the host receives a parseable
/// envelope instead of a panic stack trace.
fn print_json<T: Serialize>(value: &T, fallback: &str) {
    let s = serde_json::to_string(value).unwrap_or_else(|e| {
        eprintln!("plugin3: response serialisation failed: {e}");
        fallback.to_string()
    });
    println!("{s}");
}

// ponytail: detect_host is one-time at CLI startup per ADR-0013.
// `OnceLock` keeps it cheap on the hot path; clap hasn't parsed
// args yet so this runs before hook dispatch.
pub(crate) fn current_host() -> Host {
    static HOST: std::sync::OnceLock<Host> = std::sync::OnceLock::new();
    *HOST.get_or_init(detect_host)
}

/// One hook entry in the host's settings file. Mirrors the
/// shape Claude Code expects (ADR-0009 § Implementation notes).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CommandHook {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub command: String,
    pub timeout: u64,
}

/// All three hook slots, one per host event. A `None` slot means
/// the host doesn't register that hook (a host without
/// `PreCompact`, say, gets only `PostToolUse` + `UserPromptSubmit`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct HookConfig {
    #[serde(rename = "PostToolUse", skip_serializing_if = "Option::is_none")]
    pub post_tool_use: Option<Vec<CommandHook>>,
    #[serde(rename = "UserPromptSubmit", skip_serializing_if = "Option::is_none")]
    pub user_prompt_submit: Option<Vec<CommandHook>>,
    #[serde(rename = "PreCompact", skip_serializing_if = "Option::is_none")]
    pub pre_compact: Option<Vec<CommandHook>>,
}

// ponytail: ADR-0009 § Implementation notes prescribes a registry
// so `plugin3 init` (a future subcommand) writes the JSON, not
// the user copy-pasting from this ADR. Today's only host with a
// real settings file is Claude Code; the others return None
// slots so a future contributor adding Cursor/Aider fills in
// one arm at a time.
pub(crate) fn register_hooks(host: Host) -> HookConfig {
    match host {
        Host::ClaudeCode => HookConfig {
            post_tool_use: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook post-tool-use".into(),
                timeout: 5,
            }]),
            user_prompt_submit: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook user-prompt-submit".into(),
                timeout: 2,
            }]),
            pre_compact: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook pre-compact".into(),
                timeout: 10,
            }]),
        },
        // Cursor/Aider slots stay None — their host settings
        // formats are not yet wired (ADR-0009 § Implementation
        // notes names them but defers the JSON to a future ADR).
        _ => HookConfig::default(),
    }
}

pub(crate) fn post_tool_use() {
    // ponytail: `current_host()` is one-time cached via `OnceLock`,
    // and its sole consumer is `self_check()` (which calls
    // `register_hooks(current_host())` on a different code path).
    // The hook dispatcher doesn't branch on host — every host gets
    // the same PostToolUse shape. No need to warm the cache here.
    let Some(payload) = read_stdin_json::<PostToolUsePayload>() else {
        // ADR-0009: passthrough with a note; do not crash the host.
        let resp = PostToolUseResponse {
            content: String::new(),
            note: Some("plugin3: stdin parse failed; passing through".into()),
        };
        print_json(
            &resp,
            r#"{"content":"","note":"plugin3: response serialisation failed"}"#,
        );
        return;
    };
    let bytes_in = payload.content.len();
    // ponytail: ADR-0007 — route through the orchestrator so the
    // detect→decide→slice fan-out lives in one place. The CLI maps
    // the orchestrator's `SliceDecision` back into the PostToolUse
    // response shape; everything else (note text, recent-output
    // bookkeeping, usage emission) stays as it was.
    let store = open_store();
    let slicer = HeadTailSlicer::default();
    let orch = SlicingOrchestrator {
        store: store.as_ref(),
        slicer: &slicer,
        detector: DetectorCache::new(),
    };
    let result = run_orchestrator(
        &orch,
        &[(
            payload.tool_result_key.clone(),
            payload.content.clone(),
            Some(payload.tool_name.clone()),
        )],
    );
    let Some((_, decision)) = result.decisions.into_iter().next() else {
        // ponytail: this branch should be unreachable today (the
        // orchestrator returns one decision per input). Treat it as
        // a pass-through rather than panicking inside the host hook
        // — a future orchestrator change must not crash Claude
        // Code's PostToolUse handler.
        eprintln!("plugin3: orchestrator returned no decision; passing input through");
        let resp = PostToolUseResponse {
            content: payload.content.clone(),
            note: Some("plugin3: no slicing decision produced; passing through".into()),
        };
        print_json(
            &resp,
            r#"{"content":"","note":"plugin3: response serialisation failed"}"#,
        );
        append_recent(
            &if payload.tool_result_key.is_empty() {
                "passthrough".to_string()
            } else {
                payload.tool_result_key.clone()
            },
            bytes_in,
        );
        return;
    };
    // ponytail: the orchestrator's `DetectorCache` already
    // detected the kind for the Slice/Keep decision and now
    // surfaces it on the decision itself (ADR-0007 §
    // Orchestrator API). Reading it here avoids a second
    // `detector::detect(...)` call on the PostToolUse hot
    // path — the note text is the only consumer.
    let (content, note, bytes_out, recent_key, sliced) = match decision {
        SliceDecision::Keep { bytes, .. } => (
            // ponytail: ADR-0013 says note is optional. Pass-through
            // with no note; the host already knows the original
            // content because it sent it.
            payload.content,
            None,
            bytes,
            // tool_result_key may be empty for hosts that don't tag
            // outputs; substitute the orchestrator's decision-key so
            // append_recent has something stable to track.
            if payload.tool_result_key.is_empty() {
                "passthrough".to_string()
            } else {
                payload.tool_result_key
            },
            false,
        ),
        SliceDecision::Sliced {
            kind,
            marker,
            head,
            tail,
            bytes_kept,
            ..
        } => {
            let note = Some(format!("sliced {kind:?} ({bytes_kept} bytes kept)"));
            let content = format!("{head}{marker}{tail}");
            (content, note, bytes_kept, marker, true)
        }
    };
    // Track this output so the budget guard can auto-slice the largest
    // recent output when a future turn blows the ceiling (ADR-0005).
    append_recent(&recent_key, bytes_in);
    // ponytail: emit a Slice record only when an actual slice
    // happened. Pre-fix, every PostToolUse emitted a Slice record,
    // which inflated `records` and the `plugin3 report --kind slice`
    // count — Keep decisions have `bytes_in == bytes_out`, so the
    // `bytes_saved` roll-up was already 0 (the aggregator's
    // `saturating_sub` is correct), but the record itself counted
    // as a slice event. The orchestrator invariant
    // (`total_bytes_saved_sums_only_sliced_offloaded`) treats Keep
    // rows as no-ops; the CLI now matches.
    if sliced {
        emit_usage(&UsageRecord {
            kind: UsageKind::Slice,
            session_id: payload.session_id.clone(),
            bytes_in: Some(bytes_in),
            bytes_out: Some(bytes_out),
            tool: Some(payload.tool_name),
            ..empty_record()
        });
    }
    let resp = PostToolUseResponse { content, note };
    print_json(
        &resp,
        r#"{"content":"","note":"plugin3: response serialisation failed"}"#,
    );
}

pub(crate) fn user_prompt_submit() {
    let Some(payload) = read_stdin_json::<UserPromptSubmitPayload>() else {
        // ADR-0009: default to Allow on parse failure — the host's
        // own validation catches garbage; we should not block.
        // ponytail: serialise `Intervention::Allow` directly. The
        // wire shape matches `UserPromptSubmitResponse::Allow`
        // (same `#[serde(tag = "kind", rename_all = "snake_case")]`
        // rule on both enums); using the core type here avoids a
        // second hand-written reference that would have to track
        // variant renames.
        print_json(&Intervention::Allow, r#"{"kind":"allow"}"#);
        return;
    };
    let mut b = super::load_budget();
    let mut recent = super::load_recent_outputs();
    let incoming = estimate_tokens(&payload.prompt);
    b.record(incoming);
    // ponytail: `VecDeque::make_contiguous()` is the canonical
    // stdlib way to borrow the deque's ring buffer as a `&[T]`.
    // It is O(1) when the deque is already contiguous (the
    // common case after a series of `push_back`s from a fresh
    // deque) and O(n) when the ring buffer wraps — bounded at
    // 32 entries on the UserPromptSubmit path. The `mut` on
    // the binding is forced by the method signature, not by
    // intent at the call site; the call site only reads.
    let intervention = decide(&b, incoming, recent.make_contiguous());
    // ponytail: classify_kind returns None for Intervention::Allow —
    // a healthy turn is not a "significant event" per ADR-0010 and
    // must not inflate the warnings count in `plugin3 report
    // --summary`. The Option forces the skip to be explicit at the
    // call site rather than smuggled through the kind enum.
    if let Some(kind) = classify_kind(&intervention) {
        emit_usage(&UsageRecord {
            kind,
            session_id: payload.session_id.clone(),
            tokens_used: Some(b.used),
            tokens_ceiling: Some(b.ceiling),
            ..empty_record()
        });
    }
    super::save_budget(&b);
    // ponytail: `Intervention` (plugin3-core) and
    // `UserPromptSubmitResponse` (plugin3-hosts) are byte-equivalent
    // tagged enums on the wire — both `#[serde(tag = "kind",
    // rename_all = "snake_case")]` over the same four-variant shape.
    // Serialising the core enum directly produces the exact JSON
    // shape the canonical host expects; the previous hand-written
    // 4-arm `Intervention → UserPromptSubmitResponse` match
    // duplicated the variant list (adding a 5th variant required
    // updating both enums and the match arms — easy to forget one).
    // serde derives make the rename + tag work in both enums; the
    // conversion goes away.
    print_json(&intervention, r#"{"kind":"allow"}"#);
}

#[derive(Deserialize)]
struct PreCompactPayload {
    #[serde(default)]
    history_turns: Vec<TurnPayload>,
}

#[derive(Deserialize)]
struct TurnPayload {
    index: usize,
    role: String,
    content_preview: String,
}

pub(crate) fn pre_compact() {
    let Some(payload) = read_stdin_json::<PreCompactPayload>() else {
        // ADR-0009: empty hint on parse failure; host proceeds with
        // its own compaction.
        let resp = json!({ "hint": null, "summary": "" });
        print_json(&resp, r#"{"hint":null,"summary":""}"#);
        return;
    };
    let b = super::load_budget();
    let turns: Vec<Turn> = payload
        .history_turns
        .into_iter()
        .map(|t| Turn {
            index: t.index,
            role: t.role,
            content_preview: t.content_preview,
        })
        .collect();
    let hint = compaction::build_hint(&b, &turns);
    // Run the local summary over each turn preview so the host's
    // compactor has a head-start. ADR-0008: crude heuristic, no LLM.
    let compactor = LocalSummaryCompactor::default();
    let summary_text = {
        let joined = turns
            .iter()
            .map(|t| format!("[{}] {}: {}", t.index, t.role, t.content_preview))
            .collect::<Vec<_>>()
            .join("\n");
        compactor
            .apply(&joined)
            .map(|o| o.summary)
            .unwrap_or_default()
    };
    let resp = json!({
        "hint": hint,
        "summary": summary_text,
    });
    emit_compact_hint(&b);
    print_json(&resp, r#"{"hint":null,"summary":""}"#);
}

#[cfg(test)]
mod drift_tests {
    use super::*;
    use plugin3_hosts::Host;

    // ponytail: ADR-0009 § Implementation notes documents the
    // exact JSON shape Claude Code expects. Pinning it via a
    // drift test means a contributor who renames `command` to
    // `cmd` (or `timeout` to `timeout_seconds`) fails CI before
    // a user copies the JSON into `~/.claude/settings.json` and
    // wonders why Claude Code ignores the hook.
    #[test]
    fn register_hooks_claude_code_matches_adr_shape() {
        let cfg = register_hooks(Host::ClaudeCode);
        let v = serde_json::to_value(&cfg).expect("HookConfig is JSON");

        // PostToolUse: timeout 5s, command "plugin3 hook post-tool-use".
        let post = &v["PostToolUse"][0];
        assert_eq!(post["type"], "command");
        assert_eq!(post["command"], "plugin3 hook post-tool-use");
        assert_eq!(post["timeout"], 5);

        // UserPromptSubmit: timeout 2s.
        let ups = &v["UserPromptSubmit"][0];
        assert_eq!(ups["timeout"], 2);
        assert_eq!(ups["command"], "plugin3 hook user-prompt-submit");

        // PreCompact: timeout 10s.
        let pc = &v["PreCompact"][0];
        assert_eq!(pc["timeout"], 10);
        assert_eq!(pc["command"], "plugin3 hook pre-compact");
    }

    #[test]
    fn register_hooks_cursor_and_aider_return_empty_config() {
        // ponytail: the Cursor/Aider arms return HookConfig::default()
        // so serialise() yields `{}`. A contributor who accidentally
        // emits Claude Code entries for Cursor fails the registry's
        // host-aware shape — the test catches the cross-host leak.
        let cursor = serde_json::to_value(register_hooks(Host::Cursor)).expect("Cursor serialises");
        let aider = serde_json::to_value(register_hooks(Host::Aider)).expect("Aider serialises");
        assert_eq!(
            cursor,
            serde_json::json!({}),
            "Cursor must produce empty HookConfig today (ADR-0009 defers its JSON)"
        );
        assert_eq!(
            aider,
            serde_json::json!({}),
            "Aider must produce empty HookConfig today (ADR-0009 defers its JSON)"
        );
    }

    #[test]
    fn pre_compact_wire_shape_pins_parse_failure_and_empty_history() {
        // ponytail: pin the JSON wire shape the PreCompact hook
        // emits on the two divergent paths. parse-failure emits an
        // explicit null hint (the host proceeds with its own
        // compaction, per ADR-0009 § Error contract); post-decide
        // emits the full CompactHint object plus the
        // LocalSummaryCompactor summary. Both share the "summary"
        // key with an empty-string default. A contributor who
        // renames "hint" → "adv", or who flattens the null fallback
        // to an empty object (which the host would serialise as
        // `{}` and could mistake for a hint with empty fields),
        // surfaces here before Claude Code rejects the hook.
        //
        // Substring scan via include_str! — the same pattern the
        // ADR drift tests use (literal-substring scan per
        // contract, no markdown parser). The source file is the
        // spec surface for the wire shape; pinning it inline keeps
        // the pin co-located with the function under test.
        let src = include_str!("mod.rs");

        // Positive: parse-failure path emits the null-fallback
        // shape. The explicit null distinguishes "we have no
        // hint" from "we have a hint with default fields" — a
        // distinction Claude Code's envelope parser can read on
        // its own.
        assert!(
            src.contains(r#""hint": null, "summary": """#),
            "PreCompact parse-failure path must emit \
             `{{\"hint\": null, \"summary\": \"\"}}` — the explicit \
             null tells the host 'no hint available, proceed with \
             your own compaction' (ADR-0009 § Error contract). A \
             contributor who flattens this to an empty object `{{}}` \
             or an omitted key changes the wire contract.",
        );

        // Positive: post-decide path emits the CompactHint object
        // under `hint`, NOT null. A contributor who copy-pastes
        // the null-fallback shape onto both branches makes the
        // post-decide path useless — the host receives no hint
        // even after a successful decide.
        assert!(
            src.contains(r#""hint": hint,"#) && src.contains(r#""summary": summary_text,"#),
            "PreCompact post-decide path must emit the CompactHint \
             object under `hint` and the LocalSummaryCompactor output \
             under `summary`. A contributor who re-pastes the \
             null-fallback shape onto this branch loses the hint \
             the host's compactor needs.",
        );

        // Negative (lighter): the two shape fragments above are
        // mutually exclusive — `"hint": null` is the parse-failure
        // branch's literal, `"hint": hint,` is the post-decide
        // branch's binding. Both must coexist, which means both
        // paths must remain distinct in the source. A contributor
        // who collapses both to one shape removes one of the two
        // fragments and surfaces here. (A more aggressive
        // count-exactly-once pin is not used because the test's
        // own docstring + assert message contain the literal —
        // the co-location would always over-count.)
        assert!(
            src.contains(r#""hint": null"#) && src.contains(r#""hint": hint,"#),
            "PreCompact must keep both branches distinct: \
             `hint: null` (parse-failure fallback) AND \
             `hint: hint,` (post-decide CompactHint). A contributor \
             who collapses them into one shape loses the wire \
             distinction Claude Code reads off the hook envelope.",
        );
    }

    #[test]
    fn print_json_fallbacks_are_host_parseable_json() {
        // ponytail: B15 hardening — every fallback envelope passed to
        // print_json must be valid JSON so the host never receives a
        // parse error when the primary serialisation path fails.
        for fallback in [
            r#"{"content":"","note":"plugin3: response serialisation failed"}"#,
            r#"{"kind":"allow"}"#,
            r#"{"hint":null,"summary":""}"#,
        ] {
            assert!(
                serde_json::from_str::<serde_json::Value>(fallback).is_ok(),
                "fallback must be valid JSON: {fallback}"
            );
        }
    }

    #[test]
    fn register_hooks_emits_only_required_slots() {
        // ponytail: a future contributor who adds an `init` slot to
        // HookConfig must update this test. The drift here is the
        // *serialised shape* — Claude Code uses three hook keys,
        // nothing else. A stray `pre_init` or `SessionStart` slot
        // surfaces here before Claude Code rejects the JSON.
        let cfg = register_hooks(Host::ClaudeCode);
        let v = serde_json::to_value(&cfg).unwrap();
        let obj = v.as_object().expect("HookConfig serialises to object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["PostToolUse", "PreCompact", "UserPromptSubmit"]);
    }
}
