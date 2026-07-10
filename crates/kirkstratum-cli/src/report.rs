use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::CompressionContext;

/// Summary of what a pipeline run would do without applying it.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct DryRunReport {
    content_type: ContentType,
    mode: Mode,
    token_budget: Option<usize>,
    max_input_size: usize,
    input_len: usize,
    bloat_ratio: f64,
    would_offload: bool,
    runs_transforms: bool,
}

impl DryRunReport {
    /// Build a dry-run report for the given input, content type, and mode.
    pub fn new(
        input: &str,
        content_type: ContentType,
        ctx: &CompressionContext,
        cfg: &kirkstratum_core::config::PipelineConfig,
        mode: Mode,
        max_input_size: usize,
    ) -> Self {
        let bloat_ratio = ctx.bloat_ratio(input);
        let would_offload = mode.offloads_bloat()
            && ctx.is_bloated(input, content_type, cfg)
            && mode.runs_transforms();
        Self {
            content_type,
            mode,
            token_budget: ctx.token_budget,
            max_input_size,
            input_len: input.len(),
            bloat_ratio,
            would_offload,
            runs_transforms: mode.runs_transforms(),
        }
    }

    /// Serialize the report to a JSON value.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "content_type": self.content_type.as_str(),
            "content_type_label": self.content_type.label(),
            "mode": self.mode.as_str(),
            "token_budget": self.token_budget,
            "max_input_size": self.max_input_size,
            "input_len": self.input_len,
            "bloat_ratio": self.bloat_ratio,
            "would_offload": self.would_offload,
            "runs_transforms": self.runs_transforms,
        })
    }

    /// Format the report as human-readable text.
    #[must_use]
    pub fn human(&self) -> String {
        format!(
            "content_type: {}\ncontent_type_label: {}\nmode: {}\ntoken_budget: {:?}\nmax_input_size: {}\ninput_len: {}\nbloat_ratio: {:.4}\nwould_offload: {}\nruns_transforms: {}\n",
            self.content_type.as_str(),
            self.content_type.label(),
            self.mode.as_str(),
            self.token_budget,
            self.max_input_size,
            self.input_len,
            self.bloat_ratio,
            self.would_offload,
            self.runs_transforms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkstratum_core::config::PipelineConfig;
    use kirkstratum_core::content::ContentType;
    use kirkstratum_core::mode::Mode;
    use kirkstratum_core::pipeline::CompressionContext;

    #[test]
    fn report_reflects_input_and_mode() {
        let cfg = PipelineConfig::default();
        let ctx = CompressionContext::default();
        let report =
            DryRunReport::new("hello", ContentType::PlainText, &ctx, &cfg, Mode::Full, 100);
        assert_eq!(report.input_len, 5);
        assert_eq!(report.mode, Mode::Full);
        assert_eq!(report.content_type, ContentType::PlainText);
        assert!(!report.would_offload);
        assert!(report.runs_transforms);
    }

    #[test]
    fn report_offloads_when_content_is_bloated() {
        let cfg = PipelineConfig::default();
        let ctx = CompressionContext::default().with_token_budget(100);
        let big_input = "x".repeat(1000);
        let report = DryRunReport::new(
            &big_input,
            ContentType::PlainText,
            &ctx,
            &cfg,
            Mode::Full,
            100,
        );
        assert!(report.would_offload);
    }

    #[test]
    fn report_does_not_offload_in_off_mode() {
        let cfg = PipelineConfig::default();
        let ctx = CompressionContext::default().with_token_budget(100);
        let big_input = "x".repeat(1000);
        let report = DryRunReport::new(
            &big_input,
            ContentType::PlainText,
            &ctx,
            &cfg,
            Mode::Off,
            100,
        );
        assert!(!report.would_offload);
        assert!(!report.runs_transforms);
    }

    #[test]
    fn to_json_contains_expected_fields() {
        let cfg = PipelineConfig::default();
        let ctx = CompressionContext::default();
        let report = DryRunReport::new("hi", ContentType::SourceCode, &ctx, &cfg, Mode::Lite, 50);
        let json = report.to_json();
        assert_eq!(json["content_type"], "source_code");
        assert_eq!(json["content_type_label"], "source code");
        assert_eq!(json["mode"], "lite");
        assert_eq!(json["input_len"], 2);
        assert_eq!(json["max_input_size"], 50);
    }

    #[test]
    fn human_includes_expected_lines() {
        let cfg = PipelineConfig::default();
        let ctx = CompressionContext::default();
        let report = DryRunReport::new("hi", ContentType::SourceCode, &ctx, &cfg, Mode::Lite, 50);
        let human = report.human();
        assert!(human.contains("content_type: source_code"));
        assert!(human.contains("content_type_label: source code"));
        assert!(human.contains("mode: lite"));
        assert!(human.contains("input_len: 2"));
    }
}
