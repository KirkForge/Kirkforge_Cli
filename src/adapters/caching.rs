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
    if config.cache_enabled {
        let cache = ResponseCache::new(true, config.cache_dir.clone());
        Box::new(CachingAdapter::new(adapter, cache, config.json_mode))
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
            while let Some(ev) = inner.recv().await {
                events.push(ev.clone());
                if tx_out.send(ev).await.is_err() {
                    break;
                }
            }
            cache.put(
                &model_name,
                &messages_owned,
                &tools_owned,
                json_mode,
                &events,
            );
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
}
