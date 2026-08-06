//! Adapter wrapper that caches and replays model response streams.
//!
//! When `ResponseCache::enabled()` is true, [`CachingAdapter::stream`]:
//!   1. Computes a content-addressed key from `(model, messages, tools, json_mode)`.
//!   2. On cache hit, replays the stored [`StreamEvent`]s into a fresh channel.
//!   3. On cache miss, calls the inner adapter, records the events, and stores
//!      them for future identical requests.
//!
//! This is used by the executor when `Config::cache_enabled` is true.

use crate::adapters::cache::ResponseCache;
use crate::adapters::ModelAdapter;
use crate::shared::{Config, Message, ModelInfo, StreamEvent, ToolDef};

/// Conditionally wrap an adapter with the response cache.
///
/// Returns the adapter unchanged when caching is disabled, so callers don't
/// have to branch on `Config::cache_enabled`.
pub fn maybe_wrap_cached(adapter: Box<dyn ModelAdapter>, config: &Config) -> Box<dyn ModelAdapter> {
    if config.model.cache_enabled {
        let cache = ResponseCache::new(true, config.model.cache_dir.clone());
        Box::new(CachingAdapter::new(adapter, cache, config.model.json_mode))
    } else {
        adapter
    }
}

/// Wrapper that adds response caching to any [`ModelAdapter`].
pub struct CachingAdapter {
    inner: Box<dyn ModelAdapter>,
    cache: ResponseCache,
    json_mode: bool,
}

impl CachingAdapter {
    /// Wrap an existing adapter with a cache.
    pub fn new(inner: Box<dyn ModelAdapter>, cache: ResponseCache, json_mode: bool) -> Self {
        Self {
            inner,
            cache,
            json_mode,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for CachingAdapter {
    fn model_info(&self) -> ModelInfo {
        self.inner.model_info()
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        self.inner.set_json_mode(json_mode);
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let model_info = self.inner.model_info();

        if let Some(events) = self
            .cache
            .get(&model_info.name, messages, tools, self.json_mode)
        {
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(events.len().max(1));
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });
            return Ok(rx);
        }

        let rx = self.inner.stream(messages, tools).await?;
        let cache = self.cache.clone();
        let model_name = model_info.name.clone();
        let messages_owned = messages.to_vec();
        let tools_owned = tools.to_vec();
        let json_mode = self.json_mode;

        let (tx_out, rx_out) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        tokio::spawn(async move {
            let mut events = Vec::new();
            let mut inner = rx;
            loop {
                tokio::select! {
                    biased;
                    _ = tx_out.closed() => break,
                    ev = inner.recv() => {
                        let Some(ev) = ev else { break; };
                        events.push(ev.clone());
                        if tx_out.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // Only cache complete streams — the final event must be Done.
            // A dropped consumer, a cancelled turn, or an adapter that exits
            // without a terminal event would otherwise poison the cache with
            // a truncated response.
            let complete = matches!(events.last(), Some(StreamEvent::Done { .. }));
            if complete {
                cache.put(
                    &model_name,
                    &messages_owned,
                    &tools_owned,
                    json_mode,
                    &events,
                );
            } else {
                tracing::trace!(
                    model = %model_name,
                    event_count = events.len(),
                    "Stream incomplete; skipping cache write"
                );
            }
        });

        Ok(rx_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ModelAdapter;
    use crate::shared::{FinishReason, ModelInfo, TokenUsage, ToolCallStyle};

    struct DummyAdapter {
        events: Vec<StreamEvent>,
        info: ModelInfo,
    }

    #[async_trait::async_trait]
    impl ModelAdapter for DummyAdapter {
        fn model_info(&self) -> ModelInfo {
            self.info.clone()
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(self.events.len().max(1));
            let events = self.events.clone();
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });
            Ok(rx)
        }
    }

    fn adapter(events: Vec<StreamEvent>) -> Box<dyn ModelAdapter> {
        Box::new(DummyAdapter {
            events,
            info: ModelInfo {
                name: "test-model".into(),
                supports_thinking: false,
                tool_call_format: ToolCallStyle::Native,
                max_context_tokens: 4096,
                recommended_temperature: 0.7,
                supports_images: false,
                supports_cache: false,
            },
        })
    }

    #[tokio::test]
    async fn caching_adapter_miss_then_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                }),
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        // First call hits the inner adapter and populates the cache.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);

        // Second call with identical inputs replays from cache.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn caching_adapter_disabled_uses_inner_every_time() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(false, Some(tmp.path().into()));
        let events = vec![StreamEvent::Text("x".into())];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        for _ in 0..2 {
            let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
            let mut got = Vec::new();
            while let Some(ev) = rx.recv().await {
                got.push(ev);
            }
            assert_eq!(got, events);
        }
    }

    #[tokio::test]
    async fn caching_adapter_skips_cache_when_consumer_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("first".into()),
            StreamEvent::Text("second".into()),
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        // Consume only the first event, then drop the receiver.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let _first = rx.recv().await;
        drop(rx);

        // Give the forwarder a moment to observe the closed receiver.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The cache must remain empty for this key; a truncated stream
        // must not be replayed on a later identical request.
        assert!(
            cache
                .get(&wrapped.model_info().name, &messages, &tools, false)
                .is_none(),
            "partial stream should not be cached"
        );
    }

    #[tokio::test]
    async fn caching_adapter_aborts_forwarder_on_consumer_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        // Inner adapter emits events one at a time (capacity-1 channel), so
        // emission of event N+1 is gated by the forwarder pulling event N.
        // `emitted` therefore counts how many events the forwarder drained.
        // With the consumer-drop abort, the forwarder stops pulling after the
        // consumer drops, so `emitted` stays small. Without the abort it would
        // drain all events.
        let emitted = Arc::new(AtomicUsize::new(0));
        struct CountingAdapter {
            emitted: Arc<AtomicUsize>,
            n: usize,
        }
        #[async_trait::async_trait]
        impl ModelAdapter for CountingAdapter {
            fn model_info(&self) -> ModelInfo {
                ModelInfo {
                    name: "count-model".into(),
                    supports_thinking: false,
                    tool_call_format: ToolCallStyle::Native,
                    max_context_tokens: 4096,
                    recommended_temperature: 0.7,
                    supports_images: false,
                    supports_cache: false,
                }
            }
            async fn stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolDef],
            ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
                let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1);
                let emitted = self.emitted.clone();
                let n = self.n;
                tokio::spawn(async move {
                    for i in 0..n {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        let ev = StreamEvent::Text(format!("ev{i}"));
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                        emitted.fetch_add(1, Ordering::SeqCst);
                    }
                });
                Ok(rx)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = Box::new(CountingAdapter {
            emitted: emitted.clone(),
            n: 10,
        });
        let wrapped = CachingAdapter::new(inner, cache, false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let _first = rx.recv().await;
        drop(rx);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let count = emitted.load(Ordering::SeqCst);
        assert!(
            count < 10,
            "forwarder should abort after consumer drop, but drained {count}/10 events"
        );
    }

    #[tokio::test]
    async fn caching_adapter_incomplete_stream_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![StreamEvent::Text("no done event".into())];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            cache
                .get(&wrapped.model_info().name, &messages, &tools, false)
                .is_none(),
            "stream without Done event should not be cached"
        );
    }

    #[tokio::test]
    async fn caching_adapter_model_info_delegates_to_inner() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = adapter(vec![]);
        let wrapped = CachingAdapter::new(inner, cache, false);
        let info = wrapped.model_info();
        assert_eq!(info.name, "test-model");
        assert_eq!(info.max_context_tokens, 4096);
    }

    #[tokio::test]
    async fn caching_adapter_set_json_mode_propagates_to_inner() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = adapter(vec![]);
        let mut wrapped = CachingAdapter::new(inner, cache, false);
        assert!(!wrapped.json_mode);
        wrapped.set_json_mode(true);
        assert!(wrapped.json_mode);
    }

    #[tokio::test]
    async fn caching_adapter_caches_with_json_mode_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, true);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_returns_cached_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.model.cache_enabled = true;
        config.model.cache_dir = Some(tmp.path().into());
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = maybe_wrap_cached(inner, &config);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_returns_inner_when_disabled() {
        let mut config = Config::default();
        config.model.cache_enabled = false;
        let events = vec![StreamEvent::Text("x".into())];
        let inner = adapter(events.clone());
        let wrapped = maybe_wrap_cached(inner, &config);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_preserves_model_info() {
        let mut config = Config::default();
        config.model.cache_enabled = true;
        let inner = adapter(vec![]);
        let wrapped = maybe_wrap_cached(inner, &config);
        let info = wrapped.model_info();
        assert_eq!(info.name, "test-model");
    }
}
