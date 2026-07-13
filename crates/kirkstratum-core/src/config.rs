use crate::content::ContentType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Embedded default pipeline config shipped with the crate.
pub const DEFAULT_TOML: &str = include_str!("../config/pipeline.toml");

/// A finite ratio in the range `[0.0, 1.0]`.
///
/// Uses `f64` internally for clean TOML roundtrips and precise comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "f64")]
pub struct Ratio(f64);

impl Ratio {
    /// Construct a `Ratio` without validating the range.
    ///
    /// Only intended for tests and const-like construction; prefer
    /// `Ratio::try_from` for user input.
    #[must_use]
    pub const fn new_unchecked(value: f64) -> Self {
        Self(value)
    }

    /// Return the wrapped value.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for Ratio {
    fn default() -> Self {
        Self(0.0)
    }
}

impl Serialize for Ratio {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl TryFrom<f64> for Ratio {
    type Error = String;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(format!(
                "ratio must be a finite value between 0.0 and 1.0, got {value}"
            ))
        }
    }
}

/// Effective pipeline configuration after merging embedded defaults with any
/// override file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    /// Target compression ratio when reformatting content.
    #[serde(default)]
    pub reformat_target_ratio: Ratio,
    /// Threshold above which content is considered bloated and may be offloaded.
    #[serde(default)]
    pub bloat_threshold: Ratio,
    /// Fallback ratio used when reformatting offloaded content.
    #[serde(default)]
    pub offload_fallback_ratio: Ratio,
    /// Per-domain overrides keyed by detected content type.
    #[serde(default)]
    pub per_domain: HashMap<ContentType, DomainOverrides>,
    /// Maximum time, in milliseconds, that a single transform is allowed to run.
    ///
    /// A value of `0` disables the timeout and runs transforms synchronously.
    /// The default is 30 seconds.
    #[serde(default = "default_transform_timeout_ms")]
    pub transform_timeout_ms: u64,
}

/// Default per-transform timeout: 30 seconds.
fn default_transform_timeout_ms() -> u64 {
    30_000
}

impl PipelineConfig {
    /// Return the per-domain overrides for `content_type`, if any.
    ///
    /// Convenience accessor that centralizes the lookup pattern used by the
    /// pipeline and by report consumers.
    #[must_use]
    pub fn overrides_for(&self, content_type: ContentType) -> Option<&DomainOverrides> {
        self.per_domain.get(&content_type)
    }

    /// Return the effective bloat threshold for `content_type`.
    ///
    /// Falls back to the global `bloat_threshold` when no per-domain override is
    /// configured.
    #[must_use]
    pub fn bloat_threshold_for(&self, content_type: ContentType) -> f64 {
        self.overrides_for(content_type)
            .and_then(|d| d.bloat_threshold)
            .map_or(self.bloat_threshold.get(), Ratio::get)
    }

    /// Return the effective per-transform timeout in milliseconds.
    ///
    /// A value of `0` disables the timeout.
    #[must_use]
    pub fn transform_timeout_ms(&self) -> u64 {
        self.transform_timeout_ms
    }
}

/// Override values for a single content type.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DomainOverrides {
    /// Per-domain bloat threshold.
    pub bloat_threshold: Option<Ratio>,
    /// Per-domain reformat target ratio.
    pub reformat_target_ratio: Option<Ratio>,
}

/// Partial override used when loading a user config file. `Option` lets us
/// distinguish "field absent" from "field explicitly set to 0.0".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialPipelineConfig {
    #[serde(default)]
    reformat_target_ratio: Option<Ratio>,
    #[serde(default)]
    bloat_threshold: Option<Ratio>,
    #[serde(default)]
    offload_fallback_ratio: Option<Ratio>,
    #[serde(default)]
    per_domain: HashMap<ContentType, DomainOverrides>,
    #[serde(default)]
    transform_timeout_ms: Option<u64>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        // Keep this in sync with `config/pipeline.toml`. The test
        // `embedded_toml_matches_default_values` fails CI if the two drift.
        let mut per_domain = HashMap::new();
        per_domain.insert(
            ContentType::JsonObject,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.5)),
                reformat_target_ratio: None,
            },
        );
        per_domain.insert(
            ContentType::JsonArray,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.5)),
                reformat_target_ratio: None,
            },
        );
        per_domain.insert(
            ContentType::BuildOutput,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.3)),
                reformat_target_ratio: Some(Ratio::new_unchecked(0.1)),
            },
        );
        per_domain.insert(
            ContentType::GitDiff,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.6)),
                reformat_target_ratio: None,
            },
        );
        per_domain.insert(
            ContentType::SearchResults,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.4)),
                reformat_target_ratio: None,
            },
        );
        Self {
            reformat_target_ratio: Ratio::new_unchecked(0.05),
            bloat_threshold: Ratio::new_unchecked(0.5),
            offload_fallback_ratio: Ratio::new_unchecked(0.85),
            per_domain,
            transform_timeout_ms: 30_000,
        }
    }
}

impl PipelineConfig {
    /// Parse a full `PipelineConfig` from a TOML string.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::config::PipelineConfig;
    ///
    /// let cfg = PipelineConfig::from_toml(r#"
    /// bloat_threshold = 0.1
    /// "#).unwrap();
    /// assert_eq!(cfg.bloat_threshold.get(), 0.1);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if `s` is not valid TOML or contains unknown fields.
    #[must_use = "a parsed config is not useful unless consumed"]
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load an override file and merge it over the embedded defaults.
    ///
    /// Only fields present in the file are replaced; absent fields keep the
    /// default values, and explicit `0.0` values are honoured.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kirkstratum_core::config::PipelineConfig;
    ///
    /// let cfg = PipelineConfig::from_file(std::path::Path::new("pipeline.toml"));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] when the file cannot be read and
    /// [`ConfigError::Parse`] when it is not valid TOML.
    #[must_use = "a loaded config is not useful unless consumed"]
    pub fn from_file(p: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(p).map_err(|e| ConfigError::Io {
            path: p.to_path_buf(),
            source: e,
        })?;
        let partial: PartialPipelineConfig =
            toml::from_str(&text).map_err(|e| ConfigError::Parse {
                path: p.to_path_buf(),
                source: e,
            })?;
        let mut cfg = Self::default();
        cfg.apply_partial(&partial);
        Ok(cfg)
    }

    fn apply_partial(&mut self, partial: &PartialPipelineConfig) {
        if let Some(v) = partial.reformat_target_ratio {
            self.reformat_target_ratio = v;
        }
        if let Some(v) = partial.bloat_threshold {
            self.bloat_threshold = v;
        }
        if let Some(v) = partial.offload_fallback_ratio {
            self.offload_fallback_ratio = v;
        }
        if let Some(v) = partial.transform_timeout_ms {
            self.transform_timeout_ms = v;
        }
        for (ct, overrides) in &partial.per_domain {
            let entry = self.per_domain.entry(*ct).or_default();
            if let Some(v) = overrides.bloat_threshold {
                entry.bloat_threshold = Some(v);
            }
            if let Some(v) = overrides.reformat_target_ratio {
                entry.reformat_target_ratio = Some(v);
            }
        }
    }
}

/// Error produced while loading or validating a `PipelineConfig`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Could not read the config file from disk.
    #[error("cannot read config file {}: {source}", path.display())]
    Io {
        /// Path of the file that could not be read.
        path: std::path::PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Could not parse the config file as TOML.
    #[error("cannot parse config file {}: {source}", path.display())]
    Parse {
        /// Path of the file that could not be parsed.
        path: std::path::PathBuf,
        /// Underlying TOML parse error.
        source: toml::de::Error,
    },
    /// A config value failed semantic validation.
    #[error("invalid value for config field `{field}`: {message}")]
    Invalid {
        /// Field or source that produced the error.
        field: String,
        /// Human-readable explanation of the problem.
        message: String,
    },
}

impl ConfigError {
    /// Path of the file that could not be read, if this is an IO error.
    #[must_use]
    pub const fn io_path(&self) -> Option<&std::path::PathBuf> {
        match self {
            Self::Io { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Underlying IO error, if this is an IO error.
    #[must_use]
    pub const fn io_source(&self) -> Option<&std::io::Error> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }

    /// Path of the file that could not be parsed, if this is a parse error.
    #[must_use]
    pub const fn parse_path(&self) -> Option<&std::path::PathBuf> {
        match self {
            Self::Parse { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Underlying TOML parse error, if this is a parse error.
    #[must_use]
    pub const fn parse_source(&self) -> Option<&toml::de::Error> {
        match self {
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }

    /// Field or source that produced the error, if this is a semantic validation error.
    #[must_use]
    pub const fn invalid_field(&self) -> Option<&String> {
        match self {
            Self::Invalid { field, .. } => Some(field),
            _ => None,
        }
    }

    /// Human-readable explanation of the problem, if this is a semantic validation error.
    #[must_use]
    pub const fn invalid_message(&self) -> Option<&String> {
        match self {
            Self::Invalid { message, .. } => Some(message),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;
    use serde::Deserialize;

    #[test]
    fn embedded_toml_matches_default_values() {
        // This test guards against drift between `PipelineConfig::default()` and
        // the embedded `config/pipeline.toml`. If a default field changes in the
        // struct but not in the TOML, this assertion fails in CI.
        let from_default = PipelineConfig::default();
        let from_toml = PipelineConfig::from_toml(DEFAULT_TOML)
            .expect("embedded config/pipeline.toml must parse");
        assert_eq!(from_default, from_toml);
    }

    #[test]
    fn embedded_toml_roundtrip() {
        let cfg = PipelineConfig::default();
        let serialized = toml::to_string(&cfg).expect("config must serialize");
        let roundtripped =
            PipelineConfig::from_toml(&serialized).expect("serialized config must parse");
        assert_eq!(cfg, roundtripped);
    }

    #[test]
    fn ratio_rejects_out_of_range() {
        assert!(Ratio::try_from(-0.1).is_err());
        assert!(Ratio::try_from(1.1).is_err());
        assert!(Ratio::try_from(f64::NAN).is_err());
        assert!(Ratio::try_from(f64::INFINITY).is_err());
    }

    #[test]
    fn ratio_accepts_in_range() {
        let r = Ratio::try_from(0.5).unwrap();
        assert!((r.get() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn config_roundtrip_preserves_decimals() {
        let cfg = PipelineConfig::default();
        let serialized = toml::to_string(&cfg).expect("config must serialize");
        assert!(serialized.contains("reformat_target_ratio = 0.05"));
        assert!(serialized.contains("bloat_threshold = 0.5"));
        assert!(serialized.contains("offload_fallback_ratio = 0.85"));
        assert!(serialized.contains("transform_timeout_ms = 30000"));
    }

    #[test]
    fn partial_file_preserves_unset_defaults() {
        let dir = std::env::temp_dir();
        let path = dir.join("stratum-partial-test.toml");
        std::fs::write(&path, "bloat_threshold = 0.1").unwrap();

        let cfg = PipelineConfig::from_file(&path).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.1));
        assert_eq!(cfg.reformat_target_ratio, Ratio::new_unchecked(0.05));
        assert_eq!(cfg.offload_fallback_ratio, Ratio::new_unchecked(0.85));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_file_honours_explicit_zero() {
        let dir = std::env::temp_dir();
        let path = dir.join("stratum-zero-test.toml");
        std::fs::write(&path, "bloat_threshold = 0.0").unwrap();

        let cfg = PipelineConfig::from_file(&path).unwrap();

        assert_eq!(cfg.bloat_threshold, Ratio::new_unchecked(0.0));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mode_from_toml() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct WithMode {
            mode: Mode,
        }

        let full: WithMode = toml::from_str("mode = \"full\"").unwrap();
        assert_eq!(full.mode, Mode::Full);

        let off: WithMode = toml::from_str("mode = \"off\"").unwrap();
        assert_eq!(off.mode, Mode::Off);

        let ultra: WithMode = toml::from_str("mode = \"ultra\"").unwrap();
        assert_eq!(ultra.mode, Mode::Ultra);
    }

    #[test]
    fn overrides_for_returns_none_when_empty() {
        let cfg = PipelineConfig::default();
        assert_eq!(cfg.overrides_for(ContentType::PlainText), None);
    }

    #[test]
    fn overrides_for_returns_existing_entry() {
        let mut cfg = PipelineConfig::default();
        let overrides = DomainOverrides {
            bloat_threshold: Some(Ratio::new_unchecked(0.1)),
            ..Default::default()
        };
        cfg.per_domain
            .insert(ContentType::SourceCode, overrides.clone());
        assert_eq!(cfg.overrides_for(ContentType::SourceCode), Some(&overrides));
    }

    #[test]
    fn bloat_threshold_for_uses_global_when_no_override() {
        let cfg = PipelineConfig::default();
        let threshold = cfg.bloat_threshold_for(ContentType::PlainText);
        assert!((threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn bloat_threshold_for_uses_per_domain_override() {
        let mut cfg = PipelineConfig::default();
        cfg.per_domain.insert(
            ContentType::PlainText,
            DomainOverrides {
                bloat_threshold: Some(Ratio::new_unchecked(0.1)),
                ..Default::default()
            },
        );
        let threshold = cfg.bloat_threshold_for(ContentType::PlainText);
        assert!((threshold - 0.1).abs() < f64::EPSILON);

        // Other content types still fall back to the global threshold.
        assert!((cfg.bloat_threshold_for(ContentType::SourceCode) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn io_error_exposes_path_and_source() {
        let missing = std::path::PathBuf::from("/no/such/stratum/config.toml");
        let err = PipelineConfig::from_file(&missing).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
        assert_eq!(err.io_path(), Some(&missing));
        assert!(err.io_source().is_some());
        assert!(err.parse_path().is_none());
        assert!(err.invalid_field().is_none());
    }

    #[test]
    fn parse_error_exposes_path_and_source() {
        let dir = std::env::temp_dir();
        let path = dir.join("stratum-parse-error-test.toml");
        std::fs::write(&path, "not valid toml [").unwrap();

        let err = PipelineConfig::from_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert_eq!(err.parse_path(), Some(&path));
        assert!(err.parse_source().is_some());
        assert!(err.io_path().is_none());
        assert!(err.invalid_message().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_error_exposes_field_and_message() {
        let err = ConfigError::Invalid {
            field: "bloat_threshold".to_string(),
            message: "must be between 0.0 and 1.0".to_string(),
        };
        assert_eq!(err.invalid_field(), Some(&"bloat_threshold".to_string()));
        assert_eq!(
            err.invalid_message(),
            Some(&"must be between 0.0 and 1.0".to_string())
        );
        assert!(err.io_source().is_none());
        assert!(err.parse_source().is_none());
    }
}
