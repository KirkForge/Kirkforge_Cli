//! URL/path deny-list for tool access control.
//!
//! Extracted from `mod.rs`: [`DenyList`] holds compiled glob matchers for
//! denied path patterns and URL-prefix patterns. Deny-list checks are the
//! outermost access gate — they fire before any sandbox, approval, or
//! path-guard check. A denied path/URL is *always* rejected.

use std::path::Path;

/// Patterns always blocked for tool access.
///
/// Deny-list checks are the outermost gate — they fire before any
/// sandbox, approval, or guard check. A denied path is *always* rejected.
#[derive(Debug, Clone)]
pub struct DenyList {
    /// Compiled glob matchers for denied path patterns.
    path_matchers: Vec<globset::GlobMatcher>,
    /// Raw patterns (for display/debug).
    pub path_patterns: Vec<String>,
    /// URL prefix patterns (blocked if the target URL starts with any).
    pub url_patterns: Vec<String>,
}

impl DenyList {
    /// Build from raw pattern strings; invalid globs are logged and skipped.
    pub fn new(path_patterns: Vec<String>, url_patterns: Vec<String>) -> Self {
        let mut path_matchers = Vec::new();
        for p in &path_patterns {
            match globset::Glob::new(p) {
                Ok(g) => path_matchers.push(g.compile_matcher()),
                Err(e) => {
                    tracing::warn!(pattern = %p, error = %e, "invalid deny-list glob; skipping");
                }
            }
        }
        Self {
            path_matchers,
            path_patterns,
            url_patterns,
        }
    }

    /// Returns true if `path` matches any deny pattern.
    pub fn is_path_denied(&self, path: &Path) -> bool {
        let as_str = path.to_string_lossy();
        self.path_matchers
            .iter()
            .any(|m| m.is_match(as_str.as_ref()))
    }

    /// Returns true if `url` starts with any blocked prefix.
    pub fn is_url_denied(&self, url: &str) -> bool {
        url_is_denied(url, &self.url_patterns)
    }
}

/// Returns true if `url` starts with any blocked prefix in `patterns`.
pub fn url_is_denied(url: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| !p.is_empty() && url.starts_with(p))
}

impl Default for DenyList {
    fn default() -> Self {
        Self::new(
            vec![
                "**/.ssh/**".into(),
                "**/.gnupg/**".into(),
                "**/.aws/**".into(),
                "**/.git/**".into(),
                "**/__pycache__/**".into(),
                "**/.env*".into(),
                "**/*.pem".into(),
                "**/*.key".into(),
                "**/*.crt".into(),
                "**/*.cert".into(),
                "/etc/shadow".into(),
                "/etc/sudoers".into(),
                "/etc/passwd".into(),
                "/etc/kubernetes/**".into(),
            ],
            vec![
                // Cloud metadata endpoints — never let the model probe these
                "http://169.254.169.254".into(),
                "http://metadata.google.internal".into(),
                "http://100.100.100.200".into(),
            ],
        )
    }
}
