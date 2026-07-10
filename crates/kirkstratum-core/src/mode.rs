use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Pipeline mode controlling how aggressively content is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Mode {
    /// Pass input through unchanged.
    Off,
    /// Light compression; offloading disabled.
    Lite,
    /// Default balanced compression.
    Full,
    /// Keep almost everything; minimal filtering.
    Ultra,
}

/// Default mode used when no mode is specified.
pub const DEFAULT_MODE: Mode = Mode::Full;
/// All supported modes in declaration order.
pub const ALL_MODES: [Mode; 4] = [Mode::Off, Mode::Lite, Mode::Full, Mode::Ultra];

impl Mode {
    /// Return the lowercase name used in config and CLI values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    /// Whether registered transforms should run in this mode.
    #[must_use]
    pub const fn runs_transforms(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Preferred bloat threshold for this mode, if any.
    #[must_use]
    pub const fn offload_threshold(self) -> Option<f64> {
        match self {
            Self::Off => None,
            Self::Lite => Some(0.8),
            Self::Full => Some(0.5),
            Self::Ultra => Some(0.2),
        }
    }

    /// Whether the pipeline should offload bloated content in this mode.
    #[must_use]
    pub const fn offloads_bloat(self) -> bool {
        matches!(self, Self::Full | Self::Ultra)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a mode string does not match a known variant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown mode: {value}; expected one of: {}", ALL_MODES.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", "))]
pub struct ModeParseError {
    value: String,
}

impl ModeParseError {
    /// Create a [`ModeParseError`] for the unknown value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// The unknown mode value that failed to parse.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl FromStr for Mode {
    type Err = ModeParseError;

    /// Parse a pipeline mode from its lowercase string.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::mode::Mode;
    ///
    /// let mode: Mode = "full".parse().unwrap();
    /// assert_eq!(mode, Mode::Full);
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "off" => Ok(Self::Off),
            "lite" => Ok(Self::Lite),
            "full" => Ok(Self::Full),
            "ultra" => Ok(Self::Ultra),
            _ => Err(ModeParseError::new(normalized)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_from_str_roundtrips() {
        for mode in ALL_MODES {
            let parsed: Mode = mode.as_str().parse().unwrap();
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn unknown_mode_error_lists_supported_modes() {
        let err: ModeParseError = "turbo".parse::<Mode>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("turbo"));
        assert!(msg.contains("off"));
        assert!(msg.contains("lite"));
        assert!(msg.contains("full"));
        assert!(msg.contains("ultra"));
    }

    #[test]
    fn mode_parse_error_exposes_unknown_value() {
        let err: ModeParseError = "turbo".parse::<Mode>().unwrap_err();
        assert_eq!(err.value(), "turbo");
    }

    #[test]
    fn mode_parse_error_is_cloneable_and_equatable() {
        let err: ModeParseError = "turbo".parse::<Mode>().unwrap_err();
        assert_eq!(err, err.clone());
    }

    #[test]
    fn mode_from_str_is_case_insensitive_and_trims_whitespace() {
        assert_eq!("  FULL  ".parse::<Mode>().unwrap(), Mode::Full);
        assert_eq!("ULTRA".parse::<Mode>().unwrap(), Mode::Ultra);
        assert_eq!("Off".parse::<Mode>().unwrap(), Mode::Off);
    }

    #[test]
    fn mode_runs_transforms_except_off() {
        assert!(!Mode::Off.runs_transforms());
        assert!(Mode::Lite.runs_transforms());
        assert!(Mode::Full.runs_transforms());
        assert!(Mode::Ultra.runs_transforms());
    }

    #[test]
    fn mode_offloads_bloat_only_for_full_and_ultra() {
        assert!(!Mode::Off.offloads_bloat());
        assert!(!Mode::Lite.offloads_bloat());
        assert!(Mode::Full.offloads_bloat());
        assert!(Mode::Ultra.offloads_bloat());
    }

    #[test]
    fn mode_offload_threshold_matches_documented_values() {
        assert_eq!(Mode::Off.offload_threshold(), None);
        assert_eq!(Mode::Lite.offload_threshold(), Some(0.8));
        assert_eq!(Mode::Full.offload_threshold(), Some(0.5));
        assert_eq!(Mode::Ultra.offload_threshold(), Some(0.2));
    }
}
