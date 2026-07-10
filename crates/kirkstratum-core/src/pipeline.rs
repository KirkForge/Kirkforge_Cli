use crate::config::PipelineConfig;
use crate::content::ContentType;
use crate::mode::Mode;
use crate::store::OffloadStore;
use std::fmt;
use std::sync::{mpsc::RecvTimeoutError, Arc};
use std::time::Duration;
use tracing::{debug, instrument, trace, warn, Span};

/// Per-invocation context used by the bloat heuristic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct CompressionContext {
    /// Optional query string that transforms may use for relevance filtering.
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
    ///
    /// Uses `f64` so the ratio stays precise for inputs larger than ~16 MiB,
    /// where `f32` can no longer represent every byte length.
    ///
    /// Precision note: casting `usize` lengths to `f64` can lose precision for
    /// inputs larger than 2^53 bytes. That is acceptable here because this ratio
    /// is a coarse heuristic, not an exact measurement.
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
    /// use kirkstratum_core::pipeline::CompressionContext;
    ///
    /// let ctx = CompressionContext::default().with_token_budget(1024);
    /// assert_eq!(ctx.token_budget, Some(1024));
    /// ```
    pub fn with_token_budget(mut self, budget: usize) -> Self {
        self.token_budget = Some(budget.max(1));
        self
    }

    /// Set the query string that transforms may use for relevance filtering.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::pipeline::CompressionContext;
    ///
    /// let ctx = CompressionContext::default().with_query("error handling");
    /// assert_eq!(ctx.query, Some("error handling".to_string()));
    /// ```
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }
}

/// A transform over a string slice.
///
/// Implemented automatically for `Fn(&str) -> String` closures that are
/// `Send + Sync`.
pub trait Transform: Send + Sync {
    /// Apply the transform to `content` and return the result.
    #[must_use]
    fn apply(&self, content: &str) -> String;
}

impl<F: Fn(&str) -> String + Send + Sync> Transform for F {
    fn apply(&self, content: &str) -> String {
        (self)(content)
    }
}

/// Minimal orchestrator.
///
/// Registered *content* transforms run first, in registration order. Registered
/// *output* transforms run second, in registration order. The result is the
/// final string.
#[must_use]
pub struct CompressionPipeline {
    content_transforms: Vec<Arc<dyn Transform>>,
    output_transforms: Vec<Arc<dyn Transform>>,
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
            .field("output_transforms", &self.output_transforms.len())
            .finish()
    }
}

impl CompressionPipeline {
    /// Create an empty pipeline with no registered transforms.
    pub fn new() -> Self {
        Self {
            content_transforms: Vec::new(),
            output_transforms: Vec::new(),
        }
    }

    /// Register a transform that runs before bloat detection and offloading.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::pipeline::CompressionPipeline;
    ///
    /// let mut pipeline = CompressionPipeline::new();
    /// pipeline.register_content_transform(|s| s.replace('a', "A"));
    /// ```
    pub fn register_content_transform(
        &mut self,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) {
        self.content_transforms.push(Arc::new(f));
    }

    /// Register a transform that runs after bloat detection and offloading.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::pipeline::CompressionPipeline;
    ///
    /// let mut pipeline = CompressionPipeline::new();
    /// pipeline.register_output_transform(|s| s.replace('a', "A"));
    /// ```
    pub fn register_output_transform(
        &mut self,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) {
        self.output_transforms.push(Arc::new(f));
    }

    /// Run the pipeline on `content`.
    ///
    /// Content transforms run first, then optional bloat offloading, then output
    /// transforms. The result is the final string.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::config::PipelineConfig;
    /// use kirkstratum_core::content::ContentType;
    /// use kirkstratum_core::mode::Mode;
    /// use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
    /// use kirkstratum_core::store::InMemoryOffloadStore;
    ///
    /// let mut pipeline = CompressionPipeline::new();
    /// pipeline.register_content_transform(|s| s.replace('a', "A"));
    /// pipeline.register_output_transform(|s| format!("[{s}]"));
    ///
    /// let store = InMemoryOffloadStore::new();
    /// let out = pipeline.run(
    ///     "abc",
    ///     ContentType::PlainText,
    ///     &CompressionContext::default(),
    ///     &store,
    ///     &PipelineConfig::default(),
    ///     Mode::Full,
    /// );
    ///
    /// assert_eq!(out, "[Abc]");
    /// ```
    #[must_use]
    #[instrument(skip_all, fields(content_len = content.len()))]
    pub fn run(
        &self,
        content: &str,
        content_type: ContentType,
        ctx: &CompressionContext,
        store: &dyn OffloadStore,
        cfg: &PipelineConfig,
        mode: Mode,
    ) -> String {
        trace!(?content_type, ?mode, "running compression pipeline");
        let mut current = content.to_string();

        if !mode.runs_transforms() {
            trace!("mode disables transforms; returning input unchanged");
            return current;
        }

        let timeout_ms = cfg.transform_timeout_ms();

        for (i, t) in self.content_transforms.iter().enumerate() {
            let span = Span::current();
            let _stage =
                tracing::info_span!(parent: &span, "content_transform", stage = i).entered();
            let before = current.len();
            current = Self::apply_with_timeout(t, current, timeout_ms, i, "content");
            debug!(
                stage = i,
                before,
                after = current.len(),
                "ran content transform"
            );
        }

        // Offload bloated content to the store and replace it with a reference.
        if mode.offloads_bloat() && ctx.is_bloated(&current, content_type, cfg) {
            let key = store.put(&current);
            trace!(
                key,
                backend = store.backend_name(),
                len = current.len(),
                "offloaded bloated content"
            );
            current = format!("[offloaded: {key}]");
        }

        for (i, t) in self.output_transforms.iter().enumerate() {
            let span = Span::current();
            let _stage =
                tracing::info_span!(parent: &span, "output_transform", stage = i).entered();
            let before = current.len();
            current = Self::apply_with_timeout(t, current, timeout_ms, i, "output");
            debug!(
                stage = i,
                before,
                after = current.len(),
                "ran output transform"
            );
        }

        current
    }

    /// Apply `transform` to `content` with a configurable timeout.
    ///
    /// When `timeout_ms` is `0`, the transform runs synchronously. Otherwise it is
    /// executed on a background thread and the result is waited on with a channel
    /// timeout. The thread signals that it has started before the timer begins, so
    /// the timeout measures transform execution time rather than thread scheduling
    /// latency. If the transform does not finish in time (or panics), the original
    /// `content` is returned unchanged and a warning is emitted.
    fn apply_with_timeout(
        transform: &Arc<dyn Transform>,
        content: String,
        timeout_ms: u64,
        stage: usize,
        kind: &'static str,
    ) -> String {
        if timeout_ms == 0 {
            return transform.apply(&content);
        }

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let transform = Arc::clone(transform);
        // Keep a copy of the input so we can return it unchanged if the
        // transform does not finish within the timeout.
        let content_for_thread = content.clone();

        std::thread::spawn(move || {
            let _ = started_tx.send(());
            let result = transform.apply(&content_for_thread);
            let _ = result_tx.send(result);
        });

        // Wait for the worker to start before measuring the deadline. This
        // prevents false timeouts when the OS is slow to schedule the new thread.
        if started_rx
            .recv_timeout(Duration::from_millis(timeout_ms))
            .is_err()
        {
            warn!(
                stage,
                kind, timeout_ms, "transform failed to start; skipping"
            );
            return content;
        }

        match result_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                warn!(stage, kind, timeout_ms, "transform timed out; skipping");
                content
            }
            Err(RecvTimeoutError::Disconnected) => {
                warn!(stage, kind, timeout_ms, "transform aborted; skipping");
                content
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Ratio;
    use crate::mode::Mode;
    use crate::store::InMemoryOffloadStore;

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
    fn pipeline_runs_content_then_output_transforms() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(|s| s.replace('a', "A"));
        pipeline.register_output_transform(|s| format!("[{s}]"));

        let store = InMemoryOffloadStore::new();
        let out = pipeline.run(
            "abc",
            ContentType::PlainText,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );
        assert_eq!(out, "[Abc]");
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

        // 20 bytes / 10 token budget = ratio 2.0 > threshold 0.5 = bloated.
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
            bloat_threshold: Ratio::new_unchecked(0.01), // global would offload almost anything
            ..Default::default()
        };
        let overrides = crate::config::DomainOverrides {
            bloat_threshold: Some(Ratio::new_unchecked(0.5)), // plain text only offloads if >50%
            ..Default::default()
        };
        cfg.per_domain.insert(ContentType::PlainText, overrides);

        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext {
            token_budget: Some(100),
            ..Default::default()
        };

        // 9 bytes / 100 tokens = 0.09 < per-domain 0.5, so it should stay.
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
    fn off_mode_skips_transforms_and_offload() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(|_| "transformed".to_string());

        let cfg = PipelineConfig {
            bloat_threshold: Ratio::new_unchecked(0.0), // zero disables offloading
            ..Default::default()
        };
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
        // A length above 2^24 (16,777,216) cannot be represented exactly as f32.
        let big = "x".repeat(20_000_001);
        let ratio = ctx.bloat_ratio(&big);
        assert!((ratio - 20_000_001.0).abs() < f64::EPSILON);

        // With the default 4096 budget the ratio is still exact to f64 precision.
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
    fn pipeline_debug_shows_transform_counts() {
        let mut pipeline = CompressionPipeline::new();
        assert!(format!("{pipeline:?}").contains("content_transforms: 0"));
        assert!(format!("{pipeline:?}").contains("output_transforms: 0"));

        pipeline.register_content_transform(ToString::to_string);
        pipeline.register_output_transform(ToString::to_string);
        let debug = format!("{pipeline:?}");
        assert!(debug.contains("content_transforms: 1"));
        assert!(debug.contains("output_transforms: 1"));
        assert!(debug.starts_with("CompressionPipeline {"));
    }

    #[test]
    fn default_transform_timeout_is_thirty_seconds() {
        let cfg = PipelineConfig::default();
        assert_eq!(cfg.transform_timeout_ms(), 30_000);
    }

    #[test]
    fn transform_timeout_can_be_disabled() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(|_| "changed".to_string());

        let cfg = PipelineConfig {
            transform_timeout_ms: 0,
            ..Default::default()
        };
        let store = InMemoryOffloadStore::new();
        let out = pipeline.run(
            "input",
            ContentType::PlainText,
            &CompressionContext::default(),
            &store,
            &cfg,
            Mode::Full,
        );

        assert_eq!(out, "changed");
    }

    #[test]
    fn slow_transform_is_skipped_when_timeout_expires() {
        let mut pipeline = CompressionPipeline::new();
        // A transform that intentionally sleeps longer than the timeout.
        pipeline.register_content_transform(|s| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            s.to_uppercase()
        });

        let cfg = PipelineConfig {
            transform_timeout_ms: 1,
            ..Default::default()
        };
        let store = InMemoryOffloadStore::new();
        let input = "keep me";
        let out = pipeline.run(
            input,
            ContentType::PlainText,
            &CompressionContext::default(),
            &store,
            &cfg,
            Mode::Full,
        );

        // The slow transform is skipped, so the original input is returned.
        assert_eq!(out, input);
    }

    #[test]
    fn fast_transform_is_applied_despite_timeout() {
        let mut pipeline = CompressionPipeline::new();
        pipeline.register_content_transform(|s| s.to_uppercase());

        let cfg = PipelineConfig {
            transform_timeout_ms: 5_000,
            ..Default::default()
        };
        let store = InMemoryOffloadStore::new();
        let out = pipeline.run(
            "abc",
            ContentType::PlainText,
            &CompressionContext::default(),
            &store,
            &cfg,
            Mode::Full,
        );

        assert_eq!(out, "ABC");
    }
}
