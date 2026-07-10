//! `plugin3 init` — write the host's hook entries into the host's
//! settings file. Per ADR-0009 § Implementation notes. Today only
//! Claude Code has a settings-file schema; Cursor/Aider surface a
//! "not yet wired" exit code so the CLI shape stays stable across
//! host maturation.
//!
//! ponytail: the merge is a pure helper (`merge_into_settings`)
//! driven from the I/O wrapper (`run`). Splitting keeps the merge
//! logic testable without touching `$HOME` and makes the
//! "what would change?" question answerable without side effects.
//! A contributor who collapses the two would re-introduce a
//! filesystem round-trip on every merge-pin test.

use std::path::{Path, PathBuf};

use plugin3_core::atomic_write_text;
use plugin3_hosts::Host;
use serde_json::Value;

use crate::hooks;

/// Outcome of `merge_into_settings`. Lets callers (and tests)
/// answer "what would change?" without re-parsing the output.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MergeOutcome {
    /// The JSON document that should be written to disk.
    pub merged: Value,
    /// `true` if at least one of plugin3's hook entries replaced
    /// an existing entry (timeout drift, command drift, etc.).
    pub updated_own: bool,
    /// `true` if a non-plugin3 hook entry was preserved on a slot
    /// we also wrote to. Useful for `--dry-run` reporting.
    pub preserved_foreign: bool,
}

/// Pure merge — given the existing settings file (parsed; `None`
/// if absent or empty), the new `HookConfig` as JSON, and whether
/// the caller authorised overwriting conflicting plugin3 hooks,
/// return the document that should be written.
///
/// The merge rules (ADR-0009 § Implementation notes):
/// 1. Preserve every non-`hooks` top-level key the host already
///    has (`mcpServers`, `permissions`, etc.).
/// 2. Inside `hooks`, preserve every entry whose `command` does
///    NOT start with `"plugin3 "`. The user added those and we
///    do not own them.
/// 3. Replace any `plugin3 ` entry on a slot we also write to.
///    The new entry is built from `register_hooks(Host)`.
/// 4. Append our entries to the end of each slot's array (so the
///    user's hooks fire first, which is the polite ordering).
/// 5. Refuse (return `Err`) when `--force` is false AND we found
///    an existing `plugin3 ` entry with a *different* command on
///    a slot we also write to. A different command is a real
///    conflict; a different timeout is just drift, fixed silently.
pub(crate) fn merge_into_settings(
    existing: Option<&Value>,
    hook_json: &Value,
    force: bool,
) -> Result<MergeOutcome, MergeError> {
    let mut doc = existing
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let root = doc.as_object_mut().ok_or(MergeError::NotAnObject)?;

    let mut updated_own = false;
    let mut preserved_foreign = false;
    let mut conflict = None;

    // The new "hooks" value: start with whatever the host already
    // has, then overlay ours per slot.
    let mut hooks_obj = root
        .remove("hooks")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // For each slot plugin3 owns, build the merged array.
    for slot in ["PostToolUse", "UserPromptSubmit", "PreCompact"] {
        let ours = hook_json.get(slot).cloned();
        // Foreign entries: ones already in hooks_obj[slot] whose
        // command does NOT start with "plugin3 ".
        let mut merged_array: Vec<Value> = Vec::new();
        if let Some(existing_slot) = hooks_obj.get(slot).and_then(|v| v.as_array()) {
            for entry in existing_slot {
                let is_ours = entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.starts_with("plugin3 "));
                if is_ours {
                    // Existing plugin3 entry. Same command? OK to
                    // overwrite (timeout drift is the common case).
                    // Different command? Refuse unless --force.
                    let same_cmd = ours
                        .as_ref()
                        .and_then(|o| o.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|e| e.get("command"))
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| Some(c) == entry.get("command").and_then(|x| x.as_str()));
                    if !same_cmd {
                        let existing_cmd = entry.get("command").cloned().unwrap_or(Value::Null);
                        conflict = Some((slot.to_string(), existing_cmd));
                    }
                    updated_own = true;
                } else {
                    preserved_foreign = true;
                    merged_array.push(entry.clone());
                }
            }
        }
        if let Some(ours_array) = ours.and_then(|v| v.as_array().cloned()) {
            merged_array.extend(ours_array);
        }
        if !merged_array.is_empty() {
            hooks_obj.insert(slot.to_string(), Value::Array(merged_array));
        } else if hooks_obj.contains_key(slot) {
            // The existing slot had entries that were all ours and
            // we now want to leave them gone (e.g. plugin3 trimmed
            // its slot list) — drop the slot entirely.
            hooks_obj.remove(slot);
        }
    }

    if let Some((slot, existing_cmd)) = conflict {
        if !force {
            return Err(MergeError::Conflict { slot, existing_cmd });
        }
    }

    if !hooks_obj.is_empty() {
        root.insert("hooks".into(), Value::Object(hooks_obj));
    } else if root.contains_key("hooks") {
        root.remove("hooks");
    }

    Ok(MergeOutcome {
        merged: Value::Object(root.clone()),
        updated_own,
        preserved_foreign,
    })
}

/// Errors from `merge_into_settings`. The I/O wrapper maps each
/// variant to a stable exit code documented in `commands::init::run`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MergeError {
    /// Existing settings file parsed but the top level is not a
    /// JSON object (e.g. `"just a string"` or `[1,2,3]`).
    NotAnObject,
    /// A `plugin3 ` hook was found with a different `command` on
    /// a slot we also write to. `--force` accepts this.
    Conflict { slot: String, existing_cmd: Value },
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeError::NotAnObject => {
                write!(f, "settings file is not a JSON object at the top level")
            }
            MergeError::Conflict { slot, existing_cmd } => write!(
                f,
                "settings file already has a `plugin3 ` entry on {slot:?} \
                 with a different command ({existing_cmd}); pass --force to overwrite"
            ),
        }
    }
}

impl std::error::Error for MergeError {}

/// Resolve the Claude Code settings path. ponytail: we don't have
/// a per-host `Paths` extension in plugin3-core (the XDG-style
/// resolution is plugin3-specific, not host-specific), so this
/// lives in the only consumer (`commands::init`) rather than a
/// hypothetical `plugin3-hosts/src/claude_code.rs` slot.
fn claude_code_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// I/O wrapper — read the existing settings file (if any), merge
/// the host's `HookConfig` in, and write the result. `--dry-run`
/// stops after the merge and emits the JSON to stdout.
///
/// Exit codes (documented inline so the magic numbers at the
/// `std::process::exit` call sites stay close to the cause):
/// - 0  = success (wrote, dry-run printed, or no-op because the
///   existing file already has the same plugin3 entries)
/// - 1  = bad host argument (host has no settings-file schema yet)
/// - 2  = settings dir is not creatable
/// - 3  = conflict (existing plugin3 hook with a different command)
/// - 4  = other I/O error (read/write failed)
pub(crate) fn run(host: Host, dry_run: bool, force: bool, as_json: bool) -> i32 {
    let home = if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h)
    } else {
        eprintln!("plugin3 init: HOME is not set; cannot resolve settings path");
        return 4;
    };
    let path = match host {
        Host::ClaudeCode => claude_code_settings_path(&home),
        // ponytail: Cursor/Aider have no settings-file schema in
        // ADR-0009 today. The exit code (5) is distinct from the
        // other four so a user who runs `plugin3 init` on a non-
        // Claude host gets an actionable message rather than
        // silently succeeding. Future host graduation adds an arm
        // here and bumps the drift test.
        _ => {
            eprintln!(
                "plugin3 init: host {host:?} has no settings-file schema yet \
                       (ADR-0009 § Implementation notes defers it)"
            );
            return 5;
        }
    };

    // Read existing settings (if any). An absent file is fine —
    // we write a fresh document. A present but empty file is also
    // fine — it's the "fresh install, settings file created by
    // some other tool" case.
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) if s.trim().is_empty() => None,
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!(
                    "plugin3 init: settings file at {} is not valid JSON: {e}",
                    path.display()
                );
                return 4;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("plugin3 init: read {} failed: {e}", path.display());
            return 4;
        }
    };

    // Serialise the HookConfig exactly the way Claude Code reads
    // it. `register_hooks` already produces the right shape per
    // the drift test in `hooks::drift_tests`.
    let hook_json = match serde_json::to_value(register_hooks_for(host)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("plugin3 init: HookConfig serialise failed: {e}");
            return 4;
        }
    };

    let outcome = match merge_into_settings(existing.as_ref(), &hook_json, force) {
        Ok(o) => o,
        Err(MergeError::NotAnObject) => {
            eprintln!("plugin3 init: {}", MergeError::NotAnObject);
            return 4;
        }
        Err(MergeError::Conflict { slot, existing_cmd }) => {
            eprintln!("plugin3 init: conflict on {slot}: existing command = {existing_cmd}");
            return 3;
        }
    };

    // Ensure the parent dir exists so `atomic_write_text` doesn't
    // fail on a first-run install. Claude Code's own installer
    // creates `~/.claude/`, but we don't assume it.
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("plugin3 init: cannot create {}: {e}", parent.display());
            return 2;
        }
    }

    if dry_run {
        if as_json {
            let resp = serde_json::json!({
                "dry_run": true,
                "host": format!("{host:?}"),
                "path": path.display().to_string(),
                "would_write": outcome.merged,
                "updated_own": outcome.updated_own,
                "preserved_foreign": outcome.preserved_foreign,
            });
            crate::json_out::print_json(&resp);
        } else {
            println!("dry-run: would write {}:", path.display());
            crate::json_out::print_json(&outcome.merged);
        }
        return 0;
    }

    // ponytail: pretty-print the merged JSON before writing so the
    // file is human-readable in `$EDITOR`. A contributor who
    // serialises the raw `serde_json::Value::Object` (single-line
    // `{"hooks":{...}}`) saves bytes but breaks the user-facing
    // expectation that settings.json is diff-friendly.
    let pretty = match serde_json::to_string_pretty(&outcome.merged) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plugin3 init: serialise failed: {e}");
            return 4;
        }
    };
    // ponytail: `atomic_write_text` returns `()` and logs its own
    // eprintln on failure. We can't observe success directly, so
    // verify by re-reading the file after and comparing bytes.
    // The window between persist() and our re-read is tiny and
    // any concurrent writer would be a `plugin3 init` racing
    // itself — out of scope for the contract.
    atomic_write_text(&path, "settings", &pretty);
    match std::fs::read_to_string(&path) {
        Ok(back) if back == pretty => {}
        Ok(_) => {
            eprintln!(
                "plugin3 init: post-write verify failed at {}",
                path.display()
            );
            return 4;
        }
        Err(e) => {
            eprintln!(
                "plugin3 init: post-write read {} failed: {e}",
                path.display()
            );
            return 4;
        }
    }

    if as_json {
        let resp = serde_json::json!({
            "host": format!("{host:?}"),
            "path": path.display().to_string(),
            "written": true,
            "updated_own": outcome.updated_own,
            "preserved_foreign": outcome.preserved_foreign,
        });
        crate::json_out::print_json(&resp);
    } else {
        println!("wrote {:?} hooks into {}", host, path.display());
    }
    0
}

// ponytail: thin re-export of `register_hooks` so this file
// doesn't reach into `crate::hooks::register_hooks` directly. A
// future contributor who needs `init` to honour a different
// filter (e.g. omit a slot for `--minimal`) does it here without
// touching the registry contract.
fn register_hooks_for(host: Host) -> hooks::HookConfig {
    hooks::register_hooks(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin3_core::test_support::EnvGuard;
    use serde_json::json;

    fn plugin3_hooks() -> Value {
        // The shape `register_hooks(ClaudeCode)` serialises to —
        // built inline so the test doesn't depend on the registry
        // (the registry has its own drift tests).
        json!({
            "PostToolUse": [{
                "type": "command",
                "command": "plugin3 hook post-tool-use",
                "timeout": 5,
            }],
            "UserPromptSubmit": [{
                "type": "command",
                "command": "plugin3 hook user-prompt-submit",
                "timeout": 2,
            }],
            "PreCompact": [{
                "type": "command",
                "command": "plugin3 hook pre-compact",
                "timeout": 10,
            }],
        })
    }

    // ponytail: pin the empty-settings path. A fresh install
    // (no ~/.claude/settings.json yet) produces a document
    // containing only `{"hooks": { ... }}`. A contributor who
    // emits an empty `{}` here breaks the very first run.
    #[test]
    fn merge_with_no_existing_writes_fresh_hooks_block() {
        let ours = plugin3_hooks();
        let outcome = merge_into_settings(None, &ours, false).unwrap();
        assert_eq!(outcome.merged, json!({ "hooks": ours }));
        assert!(!outcome.updated_own, "no existing file → nothing updated");
        assert!(
            !outcome.preserved_foreign,
            "no existing file → no foreign hooks"
        );
    }

    // ponytail: pin the preservation of non-`hooks` keys.
    // A user with `{"mcpServers": {...}, "permissions": {...}}`
    // in their settings.json must keep those keys verbatim.
    // A contributor who drops `existing.clone()` and starts from
    // a fresh `serde_json::Map::new()` silently deletes them.
    #[test]
    fn merge_preserves_unrelated_top_level_keys() {
        let ours = plugin3_hooks();
        let existing = json!({
            "mcpServers": { "github": { "command": "gh-mcp" } },
            "permissions": { "allow": ["Read", "Glob"] },
            "someOtherKey": 42,
        });
        let outcome = merge_into_settings(Some(&existing), &ours, false).unwrap();
        // The hooks block is added.
        assert_eq!(outcome.merged["hooks"], ours);
        // Every unrelated top-level key survives.
        assert_eq!(outcome.merged["mcpServers"], existing["mcpServers"]);
        assert_eq!(outcome.merged["permissions"], existing["permissions"]);
        assert_eq!(outcome.merged["someOtherKey"], json!(42));
    }

    // ponytail: pin the foreign-hook preservation path. A user
    // who added their own `PostToolUse` hook to settings.json
    // (e.g. a personal pre-processor) must keep it after init.
    // We append our entry to the END of the array so the
    // user's hook fires first — the polite ordering for
    // "we added ourselves to someone else's config".
    #[test]
    fn merge_preserves_user_added_hook_on_same_slot() {
        let ours = plugin3_hooks();
        let existing = json!({
            "hooks": {
                "PostToolUse": [{
                    "type": "command",
                    "command": "user-defined-preprocessor",
                    "timeout": 7,
                }],
            },
        });
        let outcome = merge_into_settings(Some(&existing), &ours, false).unwrap();
        let arr = outcome.merged["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            2,
            "user hook + our hook must coexist; got {arr:?}"
        );
        // User's hook is FIRST (polite ordering).
        assert_eq!(arr[0]["command"], "user-defined-preprocessor");
        // Our hook is SECOND.
        assert_eq!(arr[1]["command"], "plugin3 hook post-tool-use");
        assert!(
            outcome.preserved_foreign,
            "preserved_foreign must be true when a foreign hook survives; got {outcome:?}"
        );
    }

    // ponytail: pin the same-command-replace path. A user who
    // already has `plugin3 hook post-tool-use` from a prior
    // install with a different timeout (or any other field drift)
    // must get the new entry, not a duplicate.
    #[test]
    fn merge_replaces_existing_plugin3_entry_with_same_command() {
        let ours = plugin3_hooks();
        let existing = json!({
            "hooks": {
                "PostToolUse": [{
                    "type": "command",
                    "command": "plugin3 hook post-tool-use",
                    "timeout": 99,  // stale timeout; we replace.
                }],
                "PreCompact": [{
                    "type": "command",
                    "command": "plugin3 hook pre-compact",
                    "timeout": 10,
                }],
            },
        });
        let outcome = merge_into_settings(Some(&existing), &ours, false).unwrap();
        let arr = outcome.merged["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            1,
            "existing plugin3 entry must be replaced, not duplicated; got {arr:?}"
        );
        assert_eq!(
            arr[0]["timeout"], 5,
            "replacement must use the new timeout from register_hooks"
        );
        assert_eq!(arr[0]["command"], "plugin3 hook post-tool-use");
        // PreCompact is identical → no replacement flag.
        assert!(
            outcome.updated_own,
            "updated_own must be true (replaced PostToolUse)"
        );
    }

    // ponytail: pin the conflict path. A user who already has a
    // `plugin3 ` entry with a DIFFERENT command (not the one we
    // generate) on a slot we also write to must get a hard error
    // unless `--force` is set. This catches the "two plugins
    // both named plugin3" situation (rare, but the merge has
    // to be conservative).
    #[test]
    fn merge_refuses_different_plugin3_command_without_force() {
        let ours = plugin3_hooks();
        let existing = json!({
            "hooks": {
                "PostToolUse": [{
                    "type": "command",
                    "command": "plugin3 hook post-tool-use --variant=foo",
                    "timeout": 5,
                }],
            },
        });
        let err = merge_into_settings(Some(&existing), &ours, false).unwrap_err();
        match err {
            MergeError::Conflict { slot, .. } => assert_eq!(slot, "PostToolUse"),
            other => panic!("expected Conflict on PostToolUse, got {other:?}"),
        }
    }

    // ponytail: pin the --force override on the same conflict.
    // With --force=true the conflict path accepts the new entry
    // and the existing one is overwritten. The exit-code
    // mapping is the I/O wrapper's job; this just confirms the
    // pure helper returns Ok and writes the new entry.
    #[test]
    fn merge_with_force_accepts_different_plugin3_command() {
        let ours = plugin3_hooks();
        let existing = json!({
            "hooks": {
                "PostToolUse": [{
                    "type": "command",
                    "command": "plugin3 hook post-tool-use --variant=foo",
                    "timeout": 5,
                }],
            },
        });
        let outcome = merge_into_settings(Some(&existing), &ours, true).unwrap();
        let arr = outcome.merged["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "force replaces, not appends; got {arr:?}");
        assert_eq!(
            arr[0]["command"], "plugin3 hook post-tool-use",
            "force must overwrite with our entry; got {:?}",
            arr[0]
        );
    }

    // ponytail: pin the empty-merge no-op. An existing settings
    // file that already has every plugin3 entry with the
    // correct shape must merge to a structurally equal document
    // (same keys, same values) — not to a duplicate-array doc.
    // A contributor who forgets to check `is_ours` and always
    // appends would surface here.
    #[test]
    fn merge_with_existing_identical_plugin3_entries_is_stable() {
        let ours = plugin3_hooks();
        let existing = json!({ "hooks": ours.clone() });
        let outcome = merge_into_settings(Some(&existing), &ours, false).unwrap();
        // The result is identical (same JSON object), not a
        // duplicated array.
        assert_eq!(
            outcome.merged["hooks"]["PostToolUse"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(outcome.merged["hooks"], ours);
        assert!(
            outcome.updated_own,
            "existing plugin3 entry → updated_own=true (replaced by the same entry)"
        );
    }

    // ponytail: pin the "existing settings file is a bare
    // string" path. Claude Code accepts only an object at the
    // top level; a present-but-non-object file is a malformed
    // config and must surface a hard error rather than silently
    // overwriting.
    #[test]
    fn merge_with_existing_top_level_string_returns_not_an_object() {
        let ours = plugin3_hooks();
        let existing = json!("just a string");
        let err = merge_into_settings(Some(&existing), &ours, false).unwrap_err();
        assert_eq!(err, MergeError::NotAnObject);
    }

    // ponytail: pin the claude_code_settings_path helper. The
    // path is a hard contract — Claude Code reads
    // `~/.claude/settings.json` and nothing else. A contributor
    // who switches to `~/.config/plugin3/settings.json` (the
    // XDG default) silently breaks every Claude Code install.
    #[test]
    fn claude_code_settings_path_resolves_to_dot_claude_settings_json() {
        let p = claude_code_settings_path(Path::new("/home/alice"));
        assert_eq!(p, PathBuf::from("/home/alice/.claude/settings.json"));
        let p_empty = claude_code_settings_path(Path::new("/"));
        assert_eq!(p_empty, PathBuf::from("/.claude/settings.json"));
    }

    // ponytail: integration pin for the I/O wrapper. The pure
    // merge tests above cover the rules; this one proves the
    // full pipeline (HOME resolution, parent-dir create, atomic
    // write, post-write verify) works on a hermetic tempdir.
    //
    // The three integration scenarios (fresh install write,
    // --dry-run no-write, conflict exit-3) live in a SINGLE
    // test function because each touches the HOME env var and
    // parallel tests would race on the set_var (the B8 race
    // documented in plugin3-gaps.md). Sequential sub-scopes
    // with separate EnvGuards serialise the writes — same
    // pattern as `paths.rs::partial_env_override_takes_effect_independently_per_var`.
    // A contributor who splits these into three `#[test]`
    // functions surfaces in CI as a flaky pass/fail on the
    // conflict test.
    #[test]
    fn run_io_wrapper_end_to_end_scenarios() {
        // ponytail: uses the shared process-global reentrant EnvGuard
        // (B8 fix) imported at module scope. HOME is not one of the
        // PLUGIN3_*_DIR vars, but EnvGuard::set accepts any key. The
        // reentrant mutex serialises the env writes across parallel
        // tests so the three sub-scenarios below don't race with other
        // tests that touch HOME or PLUGIN3_*_DIR.

        // ---- Scenario 1: fresh install + idempotent re-run ----
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let dir_str = dir.path().to_str().expect("utf8 path");
            let _g = EnvGuard::set("HOME", dir_str);

            let code = run(Host::ClaudeCode, false, false, false);
            assert_eq!(code, 0, "fresh install run() must exit 0; got {code}");

            let settings_path = dir.path().join(".claude").join("settings.json");
            assert!(
                settings_path.is_file(),
                "settings.json must exist at {}",
                settings_path.display()
            );

            let written = std::fs::read_to_string(&settings_path).unwrap();
            let parsed: Value = serde_json::from_str(&written).unwrap();
            assert_eq!(
                parsed["hooks"]["PostToolUse"][0]["command"],
                "plugin3 hook post-tool-use"
            );
            assert_eq!(parsed["hooks"]["PostToolUse"][0]["timeout"], 5);
            assert_eq!(parsed["hooks"]["UserPromptSubmit"][0]["timeout"], 2);
            assert_eq!(parsed["hooks"]["PreCompact"][0]["timeout"], 10);

            // Idempotent re-run on the same file.
            let code2 = run(Host::ClaudeCode, false, false, false);
            assert_eq!(code2, 0, "idempotent re-run must exit 0; got {code2}");
            let written2 = std::fs::read_to_string(&settings_path).unwrap();
            let parsed2: Value = serde_json::from_str(&written2).unwrap();
            assert_eq!(
                parsed2["hooks"]["PostToolUse"].as_array().unwrap().len(),
                1,
                "re-run must NOT duplicate the PostToolUse entry; got {:?}",
                parsed2["hooks"]["PostToolUse"]
            );
        } // EnvGuard drops → HOME restored.

        // ---- Scenario 2: --dry-run does not write ----
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let dir_str = dir.path().to_str().expect("utf8 path");
            let _g = EnvGuard::set("HOME", dir_str);

            let code = run(Host::ClaudeCode, true, false, false);
            assert_eq!(code, 0, "dry-run must exit 0; got {code}");

            let settings_path = dir.path().join(".claude").join("settings.json");
            assert!(
                !settings_path.exists(),
                "dry-run must NOT create settings.json; found {}",
                settings_path.display()
            );
        } // EnvGuard drops → HOME restored.

        // ---- Scenario 3: existing conflict → exit 3, --force → exit 0 ----
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let dir_str = dir.path().to_str().expect("utf8 path");
            let _g = EnvGuard::set("HOME", dir_str);

            let claude_dir = dir.path().join(".claude");
            std::fs::create_dir_all(&claude_dir).unwrap();
            let settings_path = claude_dir.join("settings.json");
            std::fs::write(
                &settings_path,
                json!({
                    "hooks": {
                        "PostToolUse": [{
                            "type": "command",
                            "command": "plugin3 hook post-tool-use --variant=foo",
                            "timeout": 5,
                        }]
                    }
                })
                .to_string(),
            )
            .unwrap();

            let code = run(Host::ClaudeCode, false, false, false);
            assert_eq!(code, 3, "conflict must exit 3; got {code}");

            let code2 = run(Host::ClaudeCode, false, true, false);
            assert_eq!(code2, 0, "--force must accept the conflict; got {code2}");
            let after = std::fs::read_to_string(&settings_path).unwrap();
            let parsed: Value = serde_json::from_str(&after).unwrap();
            assert_eq!(
                parsed["hooks"]["PostToolUse"][0]["command"], "plugin3 hook post-tool-use",
                "after --force the conflicting command must be replaced; got {:?}",
                parsed["hooks"]["PostToolUse"][0]
            );
        } // EnvGuard drops → HOME restored.
    }
}
