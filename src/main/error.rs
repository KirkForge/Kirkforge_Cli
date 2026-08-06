// Typed error categories used to pick a stable process exit code.
//
// Extracted from the binary root so `mod.rs` stays a thin router. The
// classifier and its tests move verbatim — no behaviour change.

/// Typed error categories used to pick a stable process exit code.
///
/// The previous `exit_code` implementation lowercased the error message and
/// matched substrings, which missed real sandbox denials that used phrases
/// such as "path is outside the allowed area" or "operation not permitted".
/// Centralising the classification in an enum makes the exit-code contract
/// explicit and easier to extend as more error sources become typed.
#[derive(Debug, thiserror::Error)]
pub(super) enum KirkForgeError {
    /// Model/host unreachable or DNS/connection failure.
    #[error("{0:#}")]
    ModelUnreachable(#[source] anyhow::Error),
    /// Permission denied, sandbox violation, or path blocked by policy.
    #[error("{0:#}")]
    AccessDenied(#[source] anyhow::Error),
    /// Configuration file parsing or validation failure.
    #[error("{0:#}")]
    ConfigParse(#[source] anyhow::Error),
    /// Any other failure.
    #[error("{0:#}")]
    General(#[source] anyhow::Error),
}

impl From<anyhow::Error> for KirkForgeError {
    fn from(e: anyhow::Error) -> Self {
        // Downcast migration (WO 14.3) — STARTED. These typed errors are now
        // classified by type, not string. The string probes below remain the
        // fallback for the categories that have no typed source yet:
        //   - ModelUnreachable: adapters return bare `anyhow`, no typed error.
        //   - AccessDenied (non-plugin): sandbox/path-policy denials from the
        //     session layer are still `anyhow` with varied phrasing.
        // Downcasted so far:
        //   - kf_plugin_sdk::ManifestError  -> ConfigParse
        //   - kf_plugin_host::ToolError -> AccessDenied (NotFound = the
        //     tool command isn't present at the sandboxed plugin root, i.e. a
        //     path-availability outcome after the root-gating policy).
        if e.downcast_ref::<kf_plugin_sdk::ManifestError>().is_some() {
            return KirkForgeError::ConfigParse(e);
        }
        if e.downcast_ref::<kf_plugin_host::ToolError>().is_some() {
            return KirkForgeError::AccessDenied(e);
        }

        // TODO: as more library calls return typed errors, replace these
        // string probes with further `downcast_ref` checks. See the note above
        // for what's already downcast and what still relies on string matching.
        let msg = format!("{e:#}").to_lowercase();
        if msg.contains("connection refused")
            || msg.contains("failed to connect")
            || msg.contains("dns error")
            || msg.contains("timed out")
            || msg.contains("model not found")
            || msg.contains("model unreachable")
        {
            KirkForgeError::ModelUnreachable(e)
        } else if msg.contains("denied")
            || msg.contains("permission")
            || msg.contains("sandbox")
            || msg.contains("blocked")
            || msg.contains("outside the allowed area")
            || msg.contains("not permitted")
        {
            KirkForgeError::AccessDenied(e)
        } else if msg.contains("config") && (msg.contains("parse") || msg.contains("invalid")) {
            KirkForgeError::ConfigParse(e)
        } else {
            KirkForgeError::General(e)
        }
    }
}

impl KirkForgeError {
    /// Structured exit code: 0 = success, 1 = general, 2 = bad args (clap),
    /// 3 = model unreachable, 4 = permission/sandbox denied, 5 = config parse error.
    pub(super) fn exit_code(&self) -> i32 {
        match self {
            KirkForgeError::ModelUnreachable(_) => 3,
            KirkForgeError::AccessDenied(_) => 4,
            KirkForgeError::ConfigParse(_) => 5,
            KirkForgeError::General(_) => 1,
        }
    }

    // User-facing suggestion for an error category. `General` has no hint —
    // a generic hint would be noise. The strings are &'static so they don't
    // allocate and stay short (<=2 lines on an 80-col terminal). Pinned by the
    // hint_* tests: a future edit must not silently drop the key substrings.
    pub(super) fn hint(&self) -> Option<&'static str> {
        match self {
            KirkForgeError::ModelUnreachable(_) => Some(
                "Check that the model provider is running (e.g. `ollama serve` for \
                 Ollama) or set the provider config in \
                 ~/.local/share/kf-code/config.toml. See config.toml.example for \
                 all options.",
            ),
            KirkForgeError::AccessDenied(_) => Some(
                "The sandbox or permission policy blocked this. Check the \
                 `security.permission_rules` and `sandbox` sections in config.toml, \
                 or run with `--auto-approve` for trusted commands.",
            ),
            KirkForgeError::ConfigParse(_) => Some(
                "The config file at ~/.local/share/kf-code/config.toml failed to \
                 parse. Compare against config.toml.example for the expected format.",
            ),
            KirkForgeError::General(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KirkForgeError;
    use anyhow::anyhow;
    use std::path::PathBuf;

    #[test]
    fn hint_model_unreachable_mentions_ollama() {
        let err = KirkForgeError::ModelUnreachable(anyhow!("boom"));
        let h = err.hint().expect("ModelUnreachable should have a hint");
        assert!(
            h.contains("ollama"),
            "ModelUnreachable hint must mention ollama, got: {h}"
        );
    }

    #[test]
    fn hint_access_denied_mentions_permission_rules() {
        let err = KirkForgeError::AccessDenied(anyhow!("nope"));
        let h = err.hint().expect("AccessDenied should have a hint");
        assert!(
            h.contains("permission_rules"),
            "AccessDenied hint must mention permission_rules, got: {h}"
        );
    }

    #[test]
    fn hint_config_parse_mentions_config_toml() {
        let err = KirkForgeError::ConfigParse(anyhow!("bad"));
        let h = err.hint().expect("ConfigParse should have a hint");
        assert!(
            h.contains("config.toml"),
            "ConfigParse hint must mention config.toml, got: {h}"
        );
    }

    #[test]
    fn hint_general_is_none() {
        let err = KirkForgeError::General(anyhow!("whatever"));
        assert!(err.hint().is_none(), "General must not have a fake hint");
    }

    #[test]
    fn downcast_manifest_error_classifies_as_config_parse() {
        let typed: kf_plugin_sdk::ManifestError =
            kf_plugin_sdk::ManifestError::UnsupportedApiVersion {
                version: "v99".into(),
            };
        let anyhow_err: anyhow::Error = typed.into();
        match KirkForgeError::from(anyhow_err) {
            KirkForgeError::ConfigParse(_) => {}
            other => panic!("ManifestError must classify as ConfigParse, got {other:?}"),
        }
    }

    #[test]
    fn downcast_tool_error_notfound_classifies_as_access_denied() {
        let typed: kf_plugin_host::ToolError =
            kf_plugin_host::ToolError::NotFound(PathBuf::from("/plugins/x/cmd"));
        let anyhow_err: anyhow::Error = typed.into();
        match KirkForgeError::from(anyhow_err) {
            KirkForgeError::AccessDenied(_) => {}
            other => panic!("ToolError::NotFound must classify as AccessDenied, got {other:?}"),
        }
    }
}
