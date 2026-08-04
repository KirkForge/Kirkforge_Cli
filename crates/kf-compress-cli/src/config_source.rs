use serde::Serialize;
use std::path::PathBuf;

/// Source of a config layer that contributed to the effective config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum ConfigSource {
    Embedded,
    Xdg { path: PathBuf },
    ConfigDir { path: PathBuf },
    Explicit { path: PathBuf },
}

impl ConfigSource {
    /// Machine-readable source kind identifier.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Xdg { .. } => "xdg",
            Self::ConfigDir { .. } => "config_dir",
            Self::Explicit { .. } => "explicit",
        }
    }

    /// Format the source for human-readable output.
    #[must_use]
    pub fn to_human(&self) -> String {
        match self {
            Self::Embedded => "embedded default".to_string(),
            Self::Xdg { path } => format!("xdg override: {}", path.display()),
            Self::ConfigDir { path } => format!("config-dir override: {}", path.display()),
            Self::Explicit { path } => format!("explicit config: {}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kind_matches_expected_identifier() {
        assert_eq!(ConfigSource::Embedded.kind(), "embedded");
        let path = PathBuf::from("/tmp/x.toml");
        assert_eq!(ConfigSource::Xdg { path: path.clone() }.kind(), "xdg");
        assert_eq!(
            ConfigSource::ConfigDir { path: path.clone() }.kind(),
            "config_dir"
        );
        assert_eq!(ConfigSource::Explicit { path }.kind(), "explicit");
    }

    #[test]
    fn to_human_includes_source_description() {
        let path = PathBuf::from("/tmp/x.toml");
        assert_eq!(ConfigSource::Embedded.to_human(), "embedded default");
        assert!(ConfigSource::Xdg { path: path.clone() }
            .to_human()
            .contains("xdg override"));
        assert!(ConfigSource::ConfigDir { path: path.clone() }
            .to_human()
            .contains("config-dir override"));
        assert!(ConfigSource::Explicit { path }
            .to_human()
            .contains("explicit config"));
    }
}
