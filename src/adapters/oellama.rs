//! Generic Ollama `/api/chat` adapter for models that stream NDJSON.
//!
//! DeepSeek, Gemini, GLM, and Kimi all use the same HTTP endpoint and
//! framing — they differ only in `ModelInfo` metadata and which
//! `OllamaNdjsonConfig` variant drives the parser. This file collapses
//! those four near-identical adapter modules into one struct plus a
//! small table of per-model profiles.
//!
//! All NDJSON framing logic lives in [`super::ollama_ndjson`]; this file
//! is just the HTTP glue and the per-adapter config selection.

use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

use super::ollama_ndjson::{self, OllamaNdjsonConfig};
use super::ModelAdapter;

/// Per-model profile: the `ModelInfo` metadata (minus `name`, which is
/// supplied at construction time) and the `OllamaNdjsonConfig` variant
/// that drives the NDJSON parser.
pub(super) struct OellamaProfile {
    thinking: bool,
    tool_call_format: ToolCallStyle,
    max_context_tokens: usize,
    recommended_temperature: f64,
    supports_images: bool,
    ndjson_config: OllamaNdjsonConfig,
}

const GLM_PROFILE: OellamaProfile = OellamaProfile {
    thinking: true,
    tool_call_format: ToolCallStyle::Native,
    max_context_tokens: 128_000,
    recommended_temperature: 0.7,
    supports_images: false, // GLM 5.1 cloud has no vision variant
    ndjson_config: OllamaNdjsonConfig::GLM,
};

const DEEPSEEK_PROFILE: OellamaProfile = OellamaProfile {
    thinking: true, // reasoning_content
    tool_call_format: ToolCallStyle::Native,
    max_context_tokens: 64_000,
    recommended_temperature: 0.6,
    supports_images: false, // DeepSeek-V4 cloud has no vision variant
    ndjson_config: OllamaNdjsonConfig::DEEPSEEK,
};

const KIMI_PROFILE: OellamaProfile = OellamaProfile {
    thinking: true,
    tool_call_format: ToolCallStyle::Native,
    max_context_tokens: 256_000,
    recommended_temperature: 0.6,
    supports_images: false,
    ndjson_config: OllamaNdjsonConfig::KIMI,
};

const GEMINI_PROFILE: OellamaProfile = OellamaProfile {
    thinking: false,
    tool_call_format: ToolCallStyle::OpenAiCompat,
    max_context_tokens: 1_000_000,
    recommended_temperature: 0.8,
    supports_images: true, // Gemini Flash 1M accepts image parts natively
    ndjson_config: OllamaNdjsonConfig::GEMINI,
};

/// Standard Ollama profile for generic models (llama, qwen, mistral, phi,
/// and any name routed to the Ollama adapter that doesn't match a known
/// dialect). No thinking channel; conservative 8K context default.
const OLLAMA_PROFILE: OellamaProfile = OellamaProfile {
    thinking: false,
    tool_call_format: ToolCallStyle::Native,
    max_context_tokens: 8192,
    recommended_temperature: 0.7,
    supports_images: false,
    ndjson_config: OllamaNdjsonConfig::OLLAMA,
};

/// Generic Ollama `/api/chat` adapter. Covers DeepSeek, Gemini, GLM, and
/// Kimi — all four share the same HTTP path and NDJSON framing; only the
/// `OellamaProfile` (model metadata + parser config) differs.
pub struct OellamaAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    json_mode: bool,
    response_format: Option<crate::shared::ResponseFormat>,
    seed: Option<u64>,
    timeout_secs: u64,
    profile: &'static OellamaProfile,
}

impl OellamaAdapter {
    pub(super) fn new(
        profile: &'static OellamaProfile,
        ollama_host: &str,
        model: &str,
        timeout_secs: u64,
    ) -> Self {
        Self {
            model: model.to_string(),
            api_base: ollama_host.trim_end_matches('/').to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            profile,
        }
    }

    // Exposed for the unified test module.
    pub fn glm(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self::new(&GLM_PROFILE, ollama_host, model, timeout_secs)
    }

    pub fn deepseek(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self::new(&DEEPSEEK_PROFILE, ollama_host, model, timeout_secs)
    }

    pub fn kimi(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self::new(&KIMI_PROFILE, ollama_host, model, timeout_secs)
    }

    pub fn gemini(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self::new(&GEMINI_PROFILE, ollama_host, model, timeout_secs)
    }

    /// Standard Ollama adapter for generic models. Used when the routing
    /// table selects `AdapterKind::Ollama` but the model name doesn't
    /// match a known dialect (glm/deepseek/gemini/kimi). Replaces the old
    /// fall-through to OpenAiCompat that broke `/api/chat` routing.
    pub fn ollama(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self::new(&OLLAMA_PROFILE, ollama_host, model, timeout_secs)
    }
}

#[async_trait::async_trait]
impl ModelAdapter for OellamaAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: self.profile.thinking,
            tool_call_format: self.profile.tool_call_format.clone(),
            max_context_tokens: self.profile.max_context_tokens,
            recommended_temperature: self.profile.recommended_temperature,
            supports_images: self.profile.supports_images,
            supports_cache: false, // Ollama's /api/chat has no cache_control
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        if json_mode {
            self.response_format = Some(crate::shared::ResponseFormat::JsonObject);
        }
    }
    fn set_response_format(&mut self, format: crate::shared::ResponseFormat) {
        self.response_format = Some(format);
    }
    fn set_seed(&mut self, seed: Option<u64>) {
        self.seed = seed;
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_ollama_chat_body(
            &self.model,
            messages,
            tools,
            true,
            self.response_format.as_ref(),
            self.seed,
        );
        let url = format!("{}/api/chat", self.api_base);

        let response = super::send_with_retry(|| async {
            self.client
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
        })
        .await?;

        // Channel size: 4096 events. The previous value of 128 was
        // too small for streaming responses from thinking models —
        // a single response can produce 200+ text chunks before the
        // executor drains the receiver, and a full channel blocks
        // `tx.send` which in turn causes the parser to bail with
        // "stream consumer dropped receiver mid-stream" warnings
        // (2026-06-11 incident, see screenshot 1/2/3). 4096 gives
        // ~20x headroom and is still small enough to bound memory.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        let ndjson_config = self.profile.ndjson_config.clone();

        tokio::spawn(async move {
            let stream = response.bytes_stream();
            ollama_ndjson::parse_ollama_ndjson_stream(tx, ndjson_config, stream).await;
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each profile: (label, constructor fn, model name, expected ModelInfo fields).
    #[test]
    fn model_info_is_correct_for_all_profiles() {
        #[derive(Clone, Copy)]
        enum Ctor {
            Glm,
            Deepseek,
            Kimi,
            Gemini,
        }
        let make = |c: Ctor| -> OellamaAdapter {
            match c {
                Ctor::Glm => OellamaAdapter::glm("http://localhost:11434", "glm-5.1", 120),
                Ctor::Deepseek => {
                    OellamaAdapter::deepseek("http://localhost:11434", "deepseek-v4", 120)
                }
                Ctor::Kimi => {
                    OellamaAdapter::kimi("http://localhost:11434", "kimi-2.7k-coder:cloud", 120)
                }
                Ctor::Gemini => {
                    OellamaAdapter::gemini("http://localhost:11434", "gemini-3.0-flash", 120)
                }
            }
        };
        // (label, ctor, expected name, thinking, tool_fmt, max_ctx, images)
        let cases: &[(&str, Ctor, &str, bool, ToolCallStyle, usize, bool)] = &[
            (
                "glm",
                Ctor::Glm,
                "glm-5.1",
                true,
                ToolCallStyle::Native,
                128_000,
                false,
            ),
            (
                "deepseek",
                Ctor::Deepseek,
                "deepseek-v4",
                true,
                ToolCallStyle::Native,
                64_000,
                false,
            ),
            (
                "kimi",
                Ctor::Kimi,
                "kimi-2.7k-coder:cloud",
                true,
                ToolCallStyle::Native,
                256_000,
                false,
            ),
            (
                "gemini",
                Ctor::Gemini,
                "gemini-3.0-flash",
                false,
                ToolCallStyle::OpenAiCompat,
                1_000_000,
                true,
            ),
        ];

        for (label, ctor, model, thinking, tool_fmt, max_ctx, images) in cases {
            let adapter = make(*ctor);
            let info = adapter.model_info();
            assert_eq!(info.name, *model, "{label}: name mismatch");
            assert_eq!(
                info.supports_thinking, *thinking,
                "{label}: thinking mismatch"
            );
            assert_eq!(
                info.tool_call_format, *tool_fmt,
                "{label}: tool_call_format mismatch"
            );
            assert_eq!(
                info.max_context_tokens, *max_ctx,
                "{label}: max_context_tokens mismatch"
            );
            assert_eq!(
                info.supports_images, *images,
                "{label}: supports_images mismatch"
            );
            assert!(!info.supports_cache, "{label}: supports_cache mismatch");
        }
    }

    #[test]
    fn constructor_strips_trailing_slash() {
        let adapter = OellamaAdapter::glm("http://localhost:11434/", "glm-5.1", 120);
        assert_eq!(adapter.api_base, "http://localhost:11434");
    }

    #[test]
    fn set_json_mode_toggles() {
        let mut adapter = OellamaAdapter::deepseek("http://localhost:11434", "deepseek-v4", 120);
        assert!(!adapter.json_mode);
        adapter.set_json_mode(true);
        assert!(adapter.json_mode);
    }

    #[test]
    fn set_seed_sets_value() {
        let mut adapter = OellamaAdapter::kimi("http://localhost:11434", "kimi-k2", 120);
        assert!(adapter.seed.is_none());
        adapter.set_seed(Some(42));
        assert_eq!(adapter.seed, Some(42));
    }
}
