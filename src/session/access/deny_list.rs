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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_deny_list_blocks_ssh() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(std::path::Path::new("/home/user/.ssh/id_rsa")));
    }

    #[test]
    fn default_deny_list_blocks_env() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(std::path::Path::new("/project/.env")));
        assert!(dl.is_path_denied(std::path::Path::new("/project/.env.local")));
    }

    #[test]
    fn default_deny_list_blocks_pem() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(std::path::Path::new("/tmp/cert.pem")));
    }

    #[test]
    fn default_deny_list_blocks_etc_shadow() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(std::path::Path::new("/etc/shadow")));
    }

    #[test]
    fn default_deny_list_allows_safe_path() {
        let dl = DenyList::default();
        assert!(!dl.is_path_denied(std::path::Path::new("/home/user/project/main.rs")));
    }

    #[test]
    fn default_deny_list_blocks_metadata_url() {
        let dl = DenyList::default();
        assert!(dl.is_url_denied("http://169.254.169.254/latest/meta-data/"));
        assert!(dl.is_url_denied("http://metadata.google.internal/computeMetadata/v1/"));
        assert!(dl.is_url_denied("http://100.100.100.200/latest/meta-data/"));
    }

    #[test]
    fn default_deny_list_allows_safe_url() {
        let dl = DenyList::default();
        assert!(!dl.is_url_denied("https://api.example.com/v1/endpoint"));
    }

    #[test]
    fn custom_deny_list_respects_custom_patterns() {
        let dl = DenyList::new(
            vec!["/tmp/secret/**".into()],
            vec!["http://evil.com".into()],
        );
        assert!(dl.is_path_denied(std::path::Path::new("/tmp/secret/key")));
        assert!(!dl.is_path_denied(std::path::Path::new("/etc/passwd")));
        assert!(dl.is_url_denied("http://evil.com/path"));
        assert!(!dl.is_url_denied("https://good.com/path"));
    }

    #[test]
    fn invalid_glob_patterns_are_skipped() {
        let dl = DenyList::new(vec!["[invalid".into(), "/valid/**".into()], vec![]);
        assert!(!dl.is_path_denied(std::path::Path::new("[invalid")));
        assert!(dl.is_path_denied(std::path::Path::new("/valid/file")));
    }

    #[test]
    fn empty_url_pattern_does_not_match() {
        let dl = DenyList::new(vec![], vec!["".into(), "http://blocked".into()]);
        assert!(!dl.is_url_denied("http://anything.com"));
        assert!(dl.is_url_denied("http://blocked/path"));
    }

    #[test]
    fn url_is_denied_standalone_function() {
        assert!(url_is_denied(
            "http://169.254.169.254/xyz",
            &["http://169.254.169.254".into()]
        ));
        assert!(!url_is_denied(
            "https://safe.com",
            &["http://169.254.169.254".into()]
        ));
        assert!(!url_is_denied("anything", &[]));
    }

    #[test]
    fn deny_list_path_patterns_field() {
        let patterns = vec!["/foo/**".into()];
        let dl = DenyList::new(patterns.clone(), vec![]);
        assert_eq!(dl.path_patterns, patterns);
    }
}
