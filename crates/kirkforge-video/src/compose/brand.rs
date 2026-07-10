//! Per-project brand theme — colors (and eventually font) read from
//! `<project>/brand.json`. Missing file → `BrandTheme::default()`. The
//! renderer consults `primary_color` for the accent used by StatCard
//! numbers, QuoteCard attributions, and EndTag slates; `palette` is the
//! ordered list of chart-bar / accent colors.
//!
//! ponytail: kept separate from `animated_explainer::BrandKit` so the
//! compose layer doesn't depend on a pipeline crate. The pipeline layer
//! is free to deserialize a richer struct and pass the relevant fields
//! into a `BrandTheme` before render.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BrandTheme {
    /// Hex like "#ffcc00". Defaults to a warm yellow that reads well on
    /// the black bg used by StatCard / QuoteCard / EndTag.
    #[serde(default = "default_primary")]
    pub primary_color: String,
    /// Ordered list of accent colors used for bar charts and other
    /// multi-color surfaces.
    #[serde(default = "default_palette")]
    pub palette: Vec<String>,
}

fn default_primary() -> String {
    "#ffcc00".into()
}
fn default_palette() -> Vec<String> {
    vec![
        "#3aa0ff".into(),
        "#ffcc00".into(),
        "#6cd07a".into(),
        "#ff5a5a".into(),
        "#bb86fc".into(),
    ]
}

impl Default for BrandTheme {
    fn default() -> Self {
        Self {
            primary_color: default_primary(),
            palette: default_palette(),
        }
    }
}

impl BrandTheme {
    /// ponytail: read `<project>/brand.json` if it exists. Bad JSON or
    /// missing file → defaults. The renderer never errors on brand; the
    /// project's brand is advisory, not a contract.
    pub fn from_project(project_dir: &Path) -> Self {
        let p = project_dir.join("brand.json");
        let Ok(raw) = std::fs::read_to_string(&p) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_primary_is_warm_yellow() {
        let t = BrandTheme::default();
        assert_eq!(t.primary_color, "#ffcc00");
        assert_eq!(t.palette.len(), 5);
    }

    #[test]
    fn from_project_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let t = BrandTheme::from_project(dir.path());
        assert_eq!(t.primary_color, "#ffcc00");
    }

    #[test]
    fn from_project_parses_primary_color() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("brand.json"),
            r##"{"primary_color": "#00ff88"}"##,
        )
        .unwrap();
        let t = BrandTheme::from_project(dir.path());
        assert_eq!(t.primary_color, "#00ff88");
        // palette falls through to default when not supplied
        assert_eq!(t.palette.len(), 5);
    }

    #[test]
    fn from_project_bad_json_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("brand.json"), "{not json").unwrap();
        let t = BrandTheme::from_project(dir.path());
        assert_eq!(t.primary_color, "#ffcc00");
    }
}
