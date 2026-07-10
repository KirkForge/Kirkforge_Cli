//! Cost reporting — usage.jsonl emission. Per ADR-0010.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::budget::Intervention;
use crate::paths::Paths;
use crate::ConfigFile;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    Slice,
    BudgetWarn,
    BudgetOver,
    CompactHint,
    Prompt,
    Response,
}

// ponytail: a previous `as_str` method duplicated the
// `rename_all = "snake_case"` rule by hand. It was called only
// from its own unit test (which pinned it against the serde
// output for parity). With serde on the same enum, the
// `serde_json::to_string` call IS the single source of truth
// for the snake_case spelling — callers who need the wire form
// call serde directly. Adding a new variant now requires
// updating only the enum body; the rename rule carries the
// rest.

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub kind: UsageKind,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_in: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_out: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_ceiling: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

fn usage_path() -> PathBuf {
    Paths::resolve().usage_log()
}

// ponytail: ADR-0010 frames usage.jsonl as "one record per
// significant event" — a healthy turn at Under state isn't one.
// Without `Allow → None`, every turn at Under state would inflate
// the warnings count in `plugin3 report --summary` (a regression
// that ships silently because the Allow arm is exercised in
// production on every healthy session). Returning Option makes
// the skip an explicit caller choice.
#[must_use]
pub fn classify_kind(intervention: &Intervention) -> Option<UsageKind> {
    match intervention {
        Intervention::Allow => None,
        Intervention::Warn { .. } => Some(UsageKind::BudgetWarn),
        Intervention::Slice { .. } => Some(UsageKind::Slice),
        Intervention::Compact { .. } => Some(UsageKind::BudgetOver),
    }
}

// ponytail: ADR-0010 § Privacy — a user who sets
// `[usage] enabled = false` in config.toml expects zero writes
// to usage.jsonl. We read the file on every emit because the
// hook handlers run across many processes (PostToolUse fires
// per tool result) and the config file is small (~50 bytes).
// Caching is premature. Path-parameterised so tests can point
// at a tempdir without touching the user's XDG config.
#[must_use]
pub fn is_usage_enabled_at(cfg_path: &std::path::Path) -> bool {
    // ponytail: missing *and* malformed config both default to
    // enabled — don't punish the user for a typo or an absent file.
    // `.unwrap_or` lets one match collapse two fall-through arms.
    std::fs::read_to_string(cfg_path)
        .ok()
        .and_then(|s| toml::from_str::<ConfigFile>(&s).ok())
        .is_none_or(|f| f.usage.enabled)
}

pub fn emit_usage(record: &UsageRecord) {
    emit_usage_at(record, &usage_path());
}

// ponytail: path-parameterised core of emit_usage. The public
// `emit_usage` is a thin wrapper that targets the user's XDG
// usage.jsonl; tests point this at a tempdir so they exercise
// the real file-append code path without touching the user's
// data dir. ADR-0010 § Tests #1.
pub(crate) fn emit_usage_at(record: &UsageRecord, path: &std::path::Path) {
    // ponytail: short-circuit before any I/O. The check is two
    // syscalls (read + close) and we save an append + fsync.
    if !is_usage_enabled_at(&Paths::resolve().config_file()) {
        return;
    }
    let line = match serde_json::to_string(&record) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plugin3: failed to serialise usage record: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // ponytail: this exists — losing usage records is not load-bearing
    // for the MVP. Replace with a fatal init when reporting becomes required.
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("plugin3: usage.jsonl open failed ({e}); dropping record");
            return;
        }
    };
    let _ = writeln!(file, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;
    use crate::{BudgetConfig, UsageConfig};

    // ponytail: pin the JSON wire format. The `report --kind=K`
    // filter and the CLI's `From<UsageKindArg> for UsageKind`
    // explicit match both depend on these exact spellings. A new
    // variant added without updating either surface fails at
    // compile time (the explicit match) or here (the wire spelling)
    // — not silently as a typo'd kind in usage.jsonl.
    #[test]
    fn usage_kind_serialises_to_snake_case() {
        for (k, expected) in [
            (UsageKind::Slice, "\"slice\""),
            (UsageKind::BudgetWarn, "\"budget_warn\""),
            (UsageKind::BudgetOver, "\"budget_over\""),
            (UsageKind::CompactHint, "\"compact_hint\""),
            (UsageKind::Prompt, "\"prompt\""),
            (UsageKind::Response, "\"response\""),
        ] {
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                expected,
                "kind {k:?} must serialise to its snake_case spelling — \
                 the rename_all rule is the single source of truth; a \
                 variant mismatch here means the JSON wire format and \
                 the `report --kind` filter disagree"
            );
        }
    }

    #[test]
    fn usage_record_round_trips_via_jsonl() {
        let r = UsageRecord {
            ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind: UsageKind::Slice,
            session_id: "abc".into(),
            bytes_in: Some(1000),
            bytes_out: Some(200),
            tokens_used: None,
            tokens_ceiling: None,
            tool: Some("cargo test".into()),
        };
        let line = serde_json::to_string(&r).unwrap();
        // None values are skipped (skip_serializing_if).
        assert!(!line.contains("tokens_used"));
        assert!(!line.contains("tokens_ceiling"));
        // Round-trip parses back to an equivalent record.
        let back: UsageRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back.kind, UsageKind::Slice);
        assert_eq!(back.bytes_in, Some(1000));
        assert_eq!(back.session_id, "abc");
    }

    // ponytail: pin the FULL set of optional fields under
    // `skip_serializing_if`. The round_trips test above only
    // checked two of five Optional fields. A contributor who
    // removes the attribute from one of them silently inflates
    // every JSONL line by `,"fieldname":null` and the file
    // grows for no gain. Pin every Optional here.
    #[test]
    fn usage_record_omits_all_none_optional_fields() {
        let r = UsageRecord {
            ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind: UsageKind::Prompt,
            session_id: "s".into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        };
        let line = serde_json::to_string(&r).unwrap();
        // Every Optional field must be ABSENT from the wire form
        // when None. None of these must appear as `null` either.
        for field in [
            "bytes_in",
            "bytes_out",
            "tokens_used",
            "tokens_ceiling",
            "tool",
        ] {
            assert!(
                !line.contains(field),
                "Optional field `{field}` with None must be skipped, \
                 not serialised as null. Line: {line}"
            );
            assert!(
                !line.contains(&format!("\"{field}\":null")),
                "Optional field `{field}` must not be serialised as null \
                 (the skip_serializing_if is the contract). Line: {line}"
            );
        }
        // And round-trip back: missing fields deserialise to None.
        let back: UsageRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back.bytes_in, None);
        assert_eq!(back.bytes_out, None);
        assert_eq!(back.tokens_used, None);
        assert_eq!(back.tokens_ceiling, None);
        assert_eq!(back.tool, None);
    }

    // ponytail: all `emit_*` integration tests live in ONE
    // sequential test. The env writes (PLUGIN3_CONFIG_DIR) are
    // process-global; parallel `#[test]` runs read each other's
    // config_file paths and produce false positives (the
    // disabled-config test reads a neighbour's empty config,
    // the gate opens, the file appears, assertion fails). Same
    // shape as the paths.rs consolidation: one test, three
    // scenarios, each with its own tempdir + EnvGuard, guards
    // dropped at end of each block.
    #[test]
    fn emit_usage_at_writes_appends_and_respects_privacy_gate() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok() {
            eprintln!("skipping: PLUGIN3_CONFIG_DIR already set in this environment");
            return;
        }

        // Scenario 1: single emit writes one JSONL line, all
        // record fields round-trip through the file.
        {
            let cfg = tempfile::tempdir().expect("cfg tempdir");
            let dir = tempfile::tempdir().expect("data tempdir");
            let path = dir.path().join("logs/usage.jsonl");
            let _guard = EnvGuard::set("PLUGIN3_CONFIG_DIR", cfg.path());

            let r = UsageRecord {
                ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                kind: UsageKind::Slice,
                session_id: "test".into(),
                bytes_in: Some(1000),
                bytes_out: Some(200),
                tokens_used: None,
                tokens_ceiling: None,
                tool: Some("cargo test".into()),
            };
            emit_usage_at(&r, &path);

            let body = std::fs::read_to_string(&path).expect("file written");
            let lines: Vec<&str> = body.lines().collect();
            assert_eq!(
                lines.len(),
                1,
                "single emit must produce exactly one line; got: {body:?}"
            );
            let parsed: UsageRecord =
                serde_json::from_str(lines[0]).expect("line is valid UsageRecord JSON");
            assert_eq!(parsed.kind, UsageKind::Slice);
            assert_eq!(parsed.session_id, "test");
            assert_eq!(parsed.bytes_in, Some(1000));
            assert_eq!(parsed.bytes_out, Some(200));
            assert_eq!(parsed.tool.as_deref(), Some("cargo test"));
        }

        // Scenario 2: JSONL append contract — three emits must
        // produce three independently parseable lines. Catches a
        // regression where `OpenOptions::create(true).write(true)`
        // would replace the file on each call.
        {
            let cfg = tempfile::tempdir().expect("cfg tempdir");
            let dir = tempfile::tempdir().expect("data tempdir");
            let path = dir.path().join("logs/usage.jsonl");
            let _guard = EnvGuard::set("PLUGIN3_CONFIG_DIR", cfg.path());

            let mk = |kind: UsageKind, session: &str| UsageRecord {
                ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                kind,
                session_id: session.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            emit_usage_at(&mk(UsageKind::Slice, "s1"), &path);
            emit_usage_at(&mk(UsageKind::BudgetWarn, "s2"), &path);
            emit_usage_at(&mk(UsageKind::BudgetOver, "s3"), &path);

            let body = std::fs::read_to_string(&path).expect("file written");
            let lines: Vec<&str> = body.lines().collect();
            assert_eq!(
                lines.len(),
                3,
                "three emits must produce three lines (JSONL append, not truncate); \
                 got {} lines, body: {body:?}",
                lines.len()
            );
            let kinds: Vec<UsageKind> = lines
                .iter()
                .map(|l| {
                    serde_json::from_str::<UsageRecord>(l)
                        .unwrap_or_else(|e| panic!("line {l:?} failed to parse: {e}"))
                })
                .map(|r| r.kind)
                .collect();
            assert_eq!(
                kinds,
                vec![
                    UsageKind::Slice,
                    UsageKind::BudgetWarn,
                    UsageKind::BudgetOver
                ],
                "kinds in order must match emit order"
            );
        }

        // Scenario 3: privacy gate — `enabled = false` in
        // config.toml must skip the write entirely. The
        // `is_usage_enabled_*` predicate tests cover the read
        // path; this covers the integration where the predicate
        // gates `emit_usage_at`. A contributor who inverts the
        // condition leaks records into the user's real
        // usage.jsonl — surfaced here, not as a leak.
        {
            let cfg = tempfile::tempdir().expect("cfg tempdir");
            let cfg_path = cfg.path().join("config.toml");
            std::fs::write(&cfg_path, "[usage]\nenabled = false\n").unwrap();
            let dir = tempfile::tempdir().expect("data tempdir");
            let path = dir.path().join("logs/usage.jsonl");
            // PLUGIN3_CONFIG_DIR points at the directory; Paths::resolve
            // reads config_dir, then config_file() joins "config.toml"
            // under it. Setting the env var to the file path would
            // make config_file() return "<file>/config.toml" — broken.
            let _guard = EnvGuard::set("PLUGIN3_CONFIG_DIR", cfg.path());

            let r = UsageRecord {
                ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                kind: UsageKind::Slice,
                session_id: "s".into(),
                bytes_in: Some(1000),
                bytes_out: Some(200),
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            emit_usage_at(&r, &path);

            // Disabled config means the file must NOT be created.
            // create_dir_all runs unconditionally (cheap), but
            // OpenOptions::create(true) is short-circuited by the
            // gate. If the file appears here, the gate is broken.
            assert!(
                !path.exists(),
                "usage.jsonl must not be written when [usage] enabled=false; \
                 found existing file at {path:?}"
            );
        }
    }

    #[test]
    fn is_usage_enabled_defaults_to_true_when_no_config() {
        // ponytail: ADR-0010 § Privacy defaults to reporting ON.
        // A user who hasn't touched config.toml must keep getting
        // records; silent opt-out would be hostile.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-config.toml");
        assert!(is_usage_enabled_at(&missing));
    }

    #[test]
    fn is_usage_enabled_respects_false_in_config_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[usage]\nenabled = false\n").unwrap();
        assert!(!is_usage_enabled_at(&cfg));
    }

    #[test]
    fn is_usage_enabled_respects_true_in_config_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[usage]\nenabled = true\n").unwrap();
        assert!(is_usage_enabled_at(&cfg));
    }

    #[test]
    fn is_usage_enabled_tolerates_malformed_config() {
        // ponytail: a typo in config.toml must NOT silently disable
        // reporting. Defaults to enabled and the user's existing
        // usage.jsonl keeps growing.
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "this is = not [ valid").unwrap();
        assert!(
            is_usage_enabled_at(&cfg),
            "malformed config must default to enabled, not punish the user"
        );
    }

    #[test]
    fn config_file_emits_usage_section_header() {
        // ponytail: pin the on-disk shape for ADR-0010 § Privacy.
        // A contributor who removes the `usage` field will surface
        // the regression here.
        let file = ConfigFile {
            budget: BudgetConfig::default(),
            usage: UsageConfig { enabled: false },
        };
        let s = toml::to_string(&file).expect("serialise");
        assert!(s.contains("[usage]"), "missing [usage] header, got: {s}");
        assert!(s.contains("enabled = false"), "got: {s}");
        let back: ConfigFile = toml::from_str(&s).expect("parse");
        assert!(!back.usage.enabled);
    }

    // ---- classify_kind: Intervention → Option<UsageKind> -----

    #[test]
    fn classify_kind_allow_returns_none() {
        // ponytail: regression guard for the Allow → BudgetWarn bug.
        // A healthy turn must NOT inflate the warnings count in
        // `plugin3 report --summary`. Removing this None makes the
        // turn look budget-pressured to a reader of usage.jsonl.
        assert_eq!(classify_kind(&Intervention::Allow), None);
    }

    #[test]
    fn classify_kind_warn_returns_budget_warn() {
        assert_eq!(
            classify_kind(&Intervention::Warn { remaining: 1000 }),
            Some(UsageKind::BudgetWarn),
        );
    }

    #[test]
    fn classify_kind_slice_returns_slice() {
        assert_eq!(
            classify_kind(&Intervention::Slice {
                target_key: "k".into(),
                slice_to: 100,
            }),
            Some(UsageKind::Slice),
        );
    }

    #[test]
    fn classify_kind_compact_returns_budget_over() {
        // ponytail: a Compact suggestion and a BudgetOver turn both
        // mean "the budget couldn't hold". Treating them as the same
        // kind lets a single filter catch both pressures.
        assert_eq!(
            classify_kind(&Intervention::Compact {
                reason: "full".into()
            }),
            Some(UsageKind::BudgetOver),
        );
    }

    // ---- ADR-0010 § Tests #1: emit writes a JSONL line. ----
    // Moved into `emit_usage_at_writes_appends_and_respects_privacy_gate`
    // above (Scenario 1). The single-emit case races with its
    // siblings on PLUGIN3_CONFIG_DIR when run in parallel — see
    // that test's comment for the consolidation rationale.

    // ponytail: env-var guard lives in `crate::test_support` now.
    // It uses a process-global reentrant mutex so parallel tests that
    // touch PLUGIN3_*_DIR cannot race, and nested guards in the same
    // thread do not deadlock. See test_support.rs for the
    // `ReentrantMutex` implementation and the B8 fix note.

    // ponytail: pin BOTH branches of EnvGuard::Drop. The
    // emit_writes_one_jsonl_line_to_target_path test above only
    // exercises the prior=None branch (it skips if the var is
    // already set). The paths.rs tests cover both branches for
    // their EnvGuard; this test mirrors that coverage here so a
    // contributor who "simplifies" the Drop to always remove_var
    // surfaces in BOTH files. Sequential single-test layout —
    // env writes are process-global, parallel runs race and
    // produce false positives.
    #[test]
    fn env_guard_restores_prior_value_some_branch() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok() {
            eprintln!("skipping: PLUGIN3_CONFIG_DIR already set in this environment");
            return;
        }
        // Seed round-trip: prior=None → unset on drop.
        {
            let _g_seed = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-seed");
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok("/tmp/cfg-seed"),
            );
        }
        assert!(
            std::env::var("PLUGIN3_CONFIG_DIR").is_err(),
            "seed EnvGuard (prior=None) must unset the env var on drop; \
             found {:?}",
            std::env::var("PLUGIN3_CONFIG_DIR").ok()
        );

        // Some-branch round-trip: prior=Some(v) → restored on drop.
        let outer_prior = "/tmp/cfg-prior";
        {
            let _g_outer = EnvGuard::set("PLUGIN3_CONFIG_DIR", outer_prior);
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok(outer_prior),
            );
            {
                let _g_inner = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-inner");
                assert_eq!(
                    std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                    Ok("/tmp/cfg-inner"),
                );
            }
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok(outer_prior),
                "EnvGuard Drop with prior=Some(v) must call set_var(key, v), \
                 NOT remove_var(key). Got {:?}, expected {:?}",
                std::env::var("PLUGIN3_CONFIG_DIR").ok(),
                Some(outer_prior),
            );
        }
        assert!(
            std::env::var("PLUGIN3_CONFIG_DIR").is_err(),
            "outer EnvGuard (prior=None) must unset the env var on drop; \
             found {:?}",
            std::env::var("PLUGIN3_CONFIG_DIR").ok()
        );
    }
}
