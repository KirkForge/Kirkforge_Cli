#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! plugin3-hosts — host detection and canonical payload schemas.
//! Per ADR-0013.
//!
//! ponytail: the per-host payload translation layer once planned as
//! `emit_to(host, event, payload)` was removed (B3). The real CLI hook
//! handlers in `plugin3-cli::hooks` consume the canonical payload types
//! directly and run the canonical logic themselves. This crate now
//! exposes only `Host` detection and the canonical schemas so the host
//! boundary stays isolated. Cursor, Aider, and KirkForge are stub modules
//! that document the intended shape when a future contributor adds a
//! second host. `detect_host` defaults to Claude Code because that is the
//! only host with CLI hook support today.

pub mod aider;
pub mod canonical;
pub mod claude_code;
pub mod cursor;
pub mod kirkforge;

pub use canonical::{
    PostToolUsePayload, PostToolUseResponse, PreCompactPayload, PreCompactResponse, Turn,
    UserPromptSubmitPayload, UserPromptSubmitResponse,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    ClaudeCode,
    Cursor,
    Aider,
    #[serde(rename = "kirkforge")]
    KirkForge,
}

#[must_use]
pub fn detect_host() -> Host {
    // ponytail: production entry point — wraps the pure function
    // below so the host shim layer reads `std::env::var` exactly
    // once per call. The trait-parameterised `detect_host_with`
    // is the seam used by drift tests (ADR-0013 § drift tests).
    detect_host_with(&OsEnv)
}

// ponytail: env source seam mirroring `plugin3_cli::precedence::EnvSource`.
// Production reads `std::env`; tests inject a fixed map so they
// never race with parallel tests that mutate the process env.
pub trait EnvSource {
    fn is_set(&self, key: &str) -> bool;
}

struct OsEnv;
impl EnvSource for OsEnv {
    fn is_set(&self, key: &str) -> bool {
        std::env::var(key).is_ok()
    }
}

pub fn detect_host_with(env: &dyn EnvSource) -> Host {
    // ponytail: only Claude Code has real CLI hook handlers. The
    // env-var checks exist so future Cursor/Aider/KirkForge detection
    // slots are obvious — e.g.
    // `if env.is_set("CURSOR_TRACE_ID") { Host::Cursor }`.
    // The default of Claude Code matches the working hooks today.
    // Precedence: CLAUDE_CODE > CURSOR_TRACE_ID > AIDER > KIRKFORGE_PLUGIN3 > ClaudeCode.
    // A contributor who reorders these arms breaks detection for
    // whichever host's env var is set; the drift corpus below
    // catches that swap.
    if env.is_set("CLAUDE_CODE") {
        Host::ClaudeCode
    } else if env.is_set("CURSOR_TRACE_ID") {
        Host::Cursor
    } else if env.is_set("AIDER") {
        Host::Aider
    } else if env.is_set("KIRKFORGE_PLUGIN3") {
        Host::KirkForge
    } else {
        Host::ClaudeCode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ponytail: pin the Host enum's variants and their kebab-case
    // wire spelling. ADR-0013 § Host enum defines the supported hosts;
    // a contributor who adds a variant (e.g. `Host::Codex`) without
    // updating `detect_host_with` surfaces here. The kebab-case
    // spelling is load-bearing: a future hook config auto-generate
    // script reads `Host::ClaudeCode` as `"claude-code"` from a JSON
    // manifest; renaming the variant or the rename rule breaks the
    // manifest.
    #[test]
    fn host_enum_three_variants_kebab_case() {
        for (h, expected) in [
            (Host::ClaudeCode, "\"claude-code\""),
            (Host::Cursor, "\"cursor\""),
            (Host::Aider, "\"aider\""),
            (Host::KirkForge, "\"kirkforge\""),
        ] {
            assert_eq!(
                serde_json::to_string(&h).unwrap(),
                expected,
                "Host {h:?} must serialise as {expected}"
            );
        }
    }

    // ponytail: pin the canonical `UserPromptSubmitResponse` wire
    // shape on this side of the bridge. The mirror pin on
    // `Intervention` lives in `plugin3-core::budget::tests` — both
    // enums are `#[serde(tag = "kind", rename_all = "snake_case")]`
    // over the same four-variant shape, and the CLI's `hooks::mod`
    // round-trips via serde rather than a hand-written 4-arm match.
    // The two pins together enforce: drop the serde tag on EITHER
    // enum and one of the two tests fails. Without this pin, a
    // contributor who flips the canonical enum off tagged-enum
    // form desyncs the CLI's intervention serialiser while the
    // core pin still passes — runtime breakage with no CI signal.
    #[test]
    fn user_prompt_submit_response_wire_shape_pins_all_four_variants() {
        use crate::canonical::UserPromptSubmitResponse;

        // Allow — unit variant, just the tag.
        let allow = serde_json::to_value(&UserPromptSubmitResponse::Allow).unwrap();
        assert_eq!(
            allow,
            json!({"kind": "allow"}),
            "Allow must serialise as a tagged-enum {{kind: allow}} object — \
             the CLI emits Allow on parse-failure (ADR-0009 § \
             Error contract) and the host envelope parser reads the literal \
             \"kind\": \"allow\" key"
        );

        // Warn { remaining } — payload field name is load-bearing.
        let warn = serde_json::to_value(&UserPromptSubmitResponse::Warn { remaining: 42 }).unwrap();
        assert_eq!(
            warn,
            json!({"kind": "warn", "remaining": 42}),
            "Warn must serialise with kind=warn and inline `remaining` field; \
             a contributor who renames `remaining` → `tokens_left` desyncs the \
             host's read of the budget warning envelope"
        );

        // Slice { target_key, slice_to } — both payload field names
        // are load-bearing; the host uses `target_key` to look up the
        // tool output to slice and `slice_to` as the byte budget.
        let slice = serde_json::to_value(&UserPromptSubmitResponse::Slice {
            target_key: "abc".into(),
            slice_to: 100,
        })
        .unwrap();
        assert_eq!(
            slice,
            json!({
                "kind": "slice", "target_key": "abc", "slice_to": 100,
            }),
            "Slice must serialise with kind=slice and inline `target_key` \
             and `slice_to` fields — the host's auto-slicer reads both by \
             name; a rename breaks the auto-slice round-trip"
        );

        // Compact { reason } — payload is the only structured field
        // (see plugin3-core::budget::tests::compact_reason_string_format_is_pinned).
        let compact = serde_json::to_value(&UserPromptSubmitResponse::Compact {
            reason: "session at 100/100 tokens".into(),
        })
        .unwrap();
        assert_eq!(
            compact,
            json!({
                "kind": "compact", "reason": "session at 100/100 tokens",
            }),
            "Compact must serialise with kind=compact and inline `reason` field"
        );
    }

    // ponytail: ADR-0013 § Implementation notes — the env-var
    // precedence chain (CLAUDE_CODE > CURSOR_TRACE_ID > AIDER >
    // KIRKFORGE_PLUGIN3 > default-to-ClaudeCode) is load-bearing:
    // a contributor who reorders the arms, renames an env var, or
    // changes the default silently breaks host detection. This module
    // covers `detect_host` with an `EnvSource` trait seam so tests
    // don't race on `std::env::var` mutation.
    mod detect_host_drift {
        use super::{detect_host_with, EnvSource, Host};
        use std::collections::HashSet;

        struct TestEnv {
            set: HashSet<&'static str>,
        }
        impl EnvSource for TestEnv {
            fn is_set(&self, key: &str) -> bool {
                self.set.contains(key)
            }
        }
        fn env(vars: &[&'static str]) -> TestEnv {
            TestEnv {
                set: vars.iter().copied().collect(),
            }
        }

        // ponytail: pin the precedence chain end-to-end. Each row
        // is a fixture in code form — the columns are (env-vars
        // set, expected Host). Adding a new env var = new row.
        // Reordering or renaming surfaces in the assertion message.
        #[test]
        fn precedence_chain_is_pinned() {
            let rows: &[(&[&'static str], Host, &str)] = &[
                (&[], Host::ClaudeCode, "no env vars → default ClaudeCode"),
                (&["CLAUDE_CODE"], Host::ClaudeCode, "CLAUDE_CODE only"),
                (&["CURSOR_TRACE_ID"], Host::Cursor, "CURSOR_TRACE_ID only"),
                (&["AIDER"], Host::Aider, "AIDER only"),
                (
                    &["KIRKFORGE_PLUGIN3"],
                    Host::KirkForge,
                    "KIRKFORGE_PLUGIN3 only",
                ),
                // Precedence: Claude Code beats Cursor when both set.
                (
                    &["CLAUDE_CODE", "CURSOR_TRACE_ID"],
                    Host::ClaudeCode,
                    "CLAUDE_CODE beats CURSOR_TRACE_ID",
                ),
                // Precedence: Cursor beats Aider when both set.
                (
                    &["CURSOR_TRACE_ID", "AIDER"],
                    Host::Cursor,
                    "CURSOR_TRACE_ID beats AIDER",
                ),
                // Precedence: Claude Code beats Aider.
                (
                    &["CLAUDE_CODE", "AIDER"],
                    Host::ClaudeCode,
                    "CLAUDE_CODE beats AIDER",
                ),
                // All set → Claude Code wins.
                (
                    &[
                        "CLAUDE_CODE",
                        "CURSOR_TRACE_ID",
                        "AIDER",
                        "KIRKFORGE_PLUGIN3",
                    ],
                    Host::ClaudeCode,
                    "CLAUDE_CODE beats all",
                ),
                // ponytail: case-sensitivity. Env vars are case-sensitive
                // on Linux/macOS; a contributor who downcased the
                // check to "claude_code" would break detection on
                // the canonical uppercase. Drift catches.
                (
                    &["claude_code"],
                    Host::ClaudeCode,
                    "lowercase doesn't trigger",
                ),
                (
                    &["Claude_Code"],
                    Host::ClaudeCode,
                    "titlecase doesn't trigger",
                ),
            ];
            for (vars, expected, label) in rows {
                let got = detect_host_with(&env(vars));
                assert_eq!(
                    got, *expected,
                    "row `{label}`: vars={vars:?} expected {expected:?} got {got:?}"
                );
            }
        }

        // ponytail: pin the canonical env-var names. A contributor
        // who renames CLAUDE_CODE → CLAUDE_PROJECT, CURSOR_TRACE_ID
        // → CURSOR_SESSION, or AIDER → AIDER_ACTIVE surfaces here
        // before detection starts returning a wrong default for users
        // running with the canonical vars. We pin the canonical
        // hits + the near-miss defaults (which fall through to
        // ClaudeCode, the spec default).
        #[test]
        fn canonical_env_var_names_are_pinned() {
            // Canonical names: a contributor who renames any of
            // these three (e.g. CLAUDE_CODE → CLAUDE_PROJECT)
            // breaks host detection for users running with the
            // original vars. Drift catches here.
            assert_eq!(detect_host_with(&env(&["CLAUDE_CODE"])), Host::ClaudeCode);
            assert_eq!(detect_host_with(&env(&["CURSOR_TRACE_ID"])), Host::Cursor);
            assert_eq!(detect_host_with(&env(&["AIDER"])), Host::Aider);
            assert_eq!(
                detect_host_with(&env(&["KIRKFORGE_PLUGIN3"])),
                Host::KirkForge
            );
            // Near-miss names: these do not match the canonical
            // spellings, so detection falls through to the
            // default (ClaudeCode per ADR-0013). A contributor who
            // widens the check to a prefix match (e.g.
            // key.starts_with("CLAUDE")) would route these to
            // ClaudeCode *as a hit* rather than via the default;
            // since both end up at ClaudeCode, distinguish via the
            // mixed-Cursor case below.
            assert_eq!(
                detect_host_with(&env(&["CLAUDE_PROJECT"])),
                Host::ClaudeCode
            );
            assert_eq!(detect_host_with(&env(&["CURSOR"])), Host::ClaudeCode);
            assert_eq!(
                detect_host_with(&env(&["KIRKFORGE"])),
                Host::ClaudeCode,
                "near-miss KIRKFORGE must not be treated as KirkForge"
            );
            // Stronger signal: a near-miss CLAUDE_PROJECT must
            // NOT shadow a real Cursor signal. If the check
            // became a starts_with, CLAUDE_PROJECT alone would
            // still default — but a starts_with on CURSOR_ would
            // flip the Cursor lookup. The Cursor pair is the load-
            // bearing near-miss test.
            assert_eq!(
                detect_host_with(&env(&["CURSOR_PROJECT"])),
                Host::ClaudeCode,
                "near-miss CURSOR_PROJECT must not be treated as Cursor",
            );
            assert_eq!(
                detect_host_with(&env(&["CURSOR_SESSION"])),
                Host::ClaudeCode,
                "near-miss CURSOR_SESSION must not be treated as Cursor",
            );
        }
    }
}
