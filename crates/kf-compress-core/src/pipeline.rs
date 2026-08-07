use crate::config::PipelineConfig;
use crate::content::ContentType;
use crate::mode::Mode;
use crate::store::OffloadStore;
use std::fmt;
use std::sync::Arc;

/// Per-invocation context used by the bloat heuristic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct CompressionContext {
    /// Optional query string for relevance filtering.
    pub query: Option<String>,
    /// Optional token budget driving the bloat ratio.
    pub token_budget: Option<usize>,
}

impl CompressionContext {
    /// Estimate whether `content` is bloated relative to the configured threshold
    /// and an optional token budget.
    #[must_use]
    pub fn is_bloated(
        &self,
        content: &str,
        content_type: ContentType,
        cfg: &PipelineConfig,
    ) -> bool {
        let threshold = cfg.bloat_threshold_for(content_type);

        if threshold <= 0.0 {
            return false;
        }

        let ratio = self.bloat_ratio(content);
        ratio > threshold
    }

    /// Simple bloat heuristic: `len / token_budget` if a budget is set, else
    /// `len / 4096` as a conservative pages-of-context proxy.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn bloat_ratio(&self, content: &str) -> f64 {
        let budget = self.token_budget.unwrap_or(4096).max(1);
        content.len() as f64 / budget as f64
    }

    /// Set the token budget used by the bloat heuristic.
    ///
    /// # Examples
    ///
    /// ```
    /// use kf_compress_core::pipeline::CompressionContext;
    ///
    /// let ctx = CompressionContext::default().with_token_budget(1024);
    /// assert_eq!(ctx.token_budget, Some(1024));
    /// ```
    pub fn with_token_budget(mut self, budget: usize) -> Self {
        self.token_budget = Some(budget.max(1));
        self
    }

    /// Set the query string for relevance filtering.
    ///
    /// # Examples
    ///
    /// ```
    /// use kf_compress_core::pipeline::CompressionContext;
    ///
    /// let ctx = CompressionContext::default().with_query("error handling");
    /// assert_eq!(ctx.query, Some("error handling".to_string()));
    /// ```
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }
}

/// A content transform applied by the pipeline.
///
/// Implementations must be pure functions: same input → same output.
pub trait Transform: Send + Sync + fmt::Debug + 'static {
    /// Transform `content` and return the result.
    fn apply(&self, content: &str, content_type: ContentType) -> String;
}

/// Boxed transform trait object.
pub type BoxedTransform = Arc<dyn Transform>;

/// Bloat-detection and offloading pipeline.
///
/// Checks whether content exceeds the configured bloat threshold relative
/// to a token budget, and offloads bloated content to the store.
/// Registered content transforms are applied before bloat checking.
pub struct CompressionPipeline {
    content_transforms: Vec<BoxedTransform>,
}

impl Default for CompressionPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CompressionPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompressionPipeline")
            .field("content_transforms", &self.content_transforms.len())
            .finish()
    }
}

impl CompressionPipeline {
    /// Create a pipeline with no transforms.
    pub fn new() -> Self {
        Self {
            content_transforms: Vec::new(),
        }
    }

    /// Register a content transform. Transforms are applied in registration order.
    pub fn register_content_transform(&mut self, transform: BoxedTransform) {
        self.content_transforms.push(transform);
    }

    /// Run the pipeline on `content`.
    ///
    /// Content transforms run first (if mode enables them), then bloat
    /// detection. If the content is not bloated after transforms, the
    /// transformed content is returned. Otherwise it is offloaded.
    ///
    /// # Examples
    ///
    /// ```
    /// use kf_compress_core::config::PipelineConfig;
    /// use kf_compress_core::content::ContentType;
    /// use kf_compress_core::mode::Mode;
    /// use kf_compress_core::pipeline::{CompressionContext, CompressionPipeline};
    /// use kf_compress_core::store::InMemoryOffloadStore;
    ///
    /// let pipeline = CompressionPipeline::new();
    /// let store = InMemoryOffloadStore::new();
    /// let out = pipeline.run(
    ///     "small content",
    ///     ContentType::PlainText,
    ///     &CompressionContext::default(),
    ///     &store,
    ///     &PipelineConfig::default(),
    ///     Mode::Full,
    /// );
    /// assert_eq!(out, "small content");
    /// ```
    #[must_use]
    pub fn run(
        &self,
        content: &str,
        content_type: ContentType,
        ctx: &CompressionContext,
        store: &dyn OffloadStore,
        cfg: &PipelineConfig,
        mode: Mode,
    ) -> String {
        if !mode.runs_transforms() || !mode.offloads_bloat() {
            return content.to_string();
        }

        let mut working = content.to_string();
        if mode.runs_transforms() {
            for transform in &self.content_transforms {
                working = transform.apply(&working, content_type);
            }
        }

        if ctx.is_bloated(&working, content_type, cfg) {
            let key = store.put(&working);
            format!("[offloaded: {key}]")
        } else {
            working
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Ratio;
    use crate::store::InMemoryOffloadStore;
    use std::sync::Arc;

    #[test]
    fn pipeline_stub_returns_input() {
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let input = "some agent context";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );
        assert_eq!(out, input);
    }

    #[test]
    fn pipeline_offloads_bloated_content() {
        let cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.5),
            ..Default::default()
        };

        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext {
            token_budget: Some(10),
            ..Default::default()
        };

        let input = "01234567890123456789";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &ctx,
            &store,
            &cfg,
            Mode::Full,
        );

        assert!(out.starts_with("[offloaded: "));
        assert!(!out.contains("0123456789"));
        assert_eq!(store.len(), 1);
        let key = out
            .trim_start_matches("[offloaded: ")
            .trim_end_matches(']')
            .to_string();
        assert_eq!(store.get(&key), Some(input.to_string()));
    }

    #[test]
    fn pipeline_keeps_small_content_unoffloaded() {
        let cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.5),
            ..Default::default()
        };
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext {
            token_budget: Some(10_000),
            ..Default::default()
        };

        let input = "small payload";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &ctx,
            &store,
            &cfg,
            Mode::Full,
        );

        assert_eq!(out, input);
        assert!(store.is_empty());
    }

    #[test]
    fn per_domain_bloat_threshold_overrides_global() {
        let mut cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.01),
            ..Default::default()
        };
        let overrides = crate::config::DomainOverrides {
            bloat_threshold: Some(Ratio::new_unchecked(0.5)),
            ..Default::default()
        };
        cfg.per_domain.insert(ContentType::PlainText, overrides);

        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext {
            token_budget: Some(100),
            ..Default::default()
        };

        let input = "some text";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &ctx,
            &store,
            &cfg,
            Mode::Full,
        );

        assert_eq!(out, input);
        assert!(store.is_empty());
    }

    #[test]
    fn off_mode_skips_offload() {
        let cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.0),
            ..Default::default()
        };
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext::default();

        let input = "hello";
        let out = pipeline.run(input, ContentType::PlainText, &ctx, &store, &cfg, Mode::Off);

        assert_eq!(out, input);
        assert!(store.is_empty());
    }

    #[test]
    fn compression_context_is_equatable() {
        let a = CompressionContext::default()
            .with_query("relevant snippet")
            .with_token_budget(1024);
        let b = CompressionContext::default()
            .with_query("relevant snippet")
            .with_token_budget(1024);
        assert_eq!(a, b);

        let c = CompressionContext::default().with_token_budget(1024);
        assert_ne!(a, c);
    }

    #[test]
    fn with_query_sets_optional_query_string() {
        let ctx = CompressionContext::default().with_query("relevant snippet");
        assert_eq!(ctx.query, Some("relevant snippet".to_string()));
        assert!(ctx.token_budget.is_none());
    }

    #[test]
    fn with_query_and_token_budget_chain() {
        let ctx = CompressionContext::default()
            .with_query("error handling")
            .with_token_budget(1024);
        assert_eq!(ctx.query, Some("error handling".to_string()));
        assert_eq!(ctx.token_budget, Some(1024));
    }

    #[test]
    fn bloat_ratio_stays_precise_for_large_inputs() {
        let ctx = CompressionContext {
            token_budget: Some(1),
            ..Default::default()
        };
        let big = "x".repeat(20_000_001);
        let ratio = ctx.bloat_ratio(&big);
        assert!((ratio - 20_000_001.0).abs() < f64::EPSILON);

        let ctx_default = CompressionContext::default();
        let ratio_default = ctx_default.bloat_ratio(&big);
        assert!((ratio_default - (20_000_001.0 / 4096.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn lite_mode_does_not_offload() {
        let cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.5),
            ..Default::default()
        };
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext {
            token_budget: Some(1),
            ..Default::default()
        };

        let input = "01234567890123456789";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &ctx,
            &store,
            &cfg,
            Mode::Lite,
        );

        assert_eq!(out, input);
        assert!(store.is_empty());
    }

    #[test]
    fn pipeline_debug_shows_name() {
        let pipeline = CompressionPipeline::new();
        let debug = format!("{pipeline:?}");
        assert!(debug.starts_with("CompressionPipeline"));
    }

    #[derive(Debug)]
    struct StripCommentsTransform;

    impl Transform for StripCommentsTransform {
        fn apply(&self, content: &str, content_type: ContentType) -> String {
            match content_type {
                ContentType::SourceCode => content
                    .lines()
                    .filter(|l| !l.trim().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => content.to_string(),
            }
        }
    }

    #[test]
    fn pipeline_applies_content_transforms() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(Arc::new(StripCommentsTransform));
        let store = InMemoryOffloadStore::new();
        let input = "fn main() {\n    // comment\n    println!(\"hi\");\n}";
        let out = pipeline.run(
            input,
            ContentType::SourceCode,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );
        assert!(!out.contains("// comment"));
        assert!(out.contains("println"));
    }

    #[test]
    fn pipeline_shrinkage_test_for_rust_source() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(Arc::new(StripCommentsTransform));
        let store = InMemoryOffloadStore::new();

        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!(
                "fn func_{i}() {{\n    // this is a comment for function {i}\n    let x = {i};\n}}\n"
            ));
        }

        let out = pipeline.run(
            &src,
            ContentType::SourceCode,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );

        // ponytail: ≥5% shrinkage — if the real number drifts, update this literal.
        let ratio = 1.0 - (out.len() as f64 / src.len() as f64);
        assert!(
            ratio >= 0.05,
            "expected ≥5% shrinkage, got {:.1}% ({} → {})",
            ratio * 100.0,
            src.len(),
            out.len(),
        );
    }
}
