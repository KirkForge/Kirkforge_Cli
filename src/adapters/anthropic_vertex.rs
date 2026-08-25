//! Anthropic Messages API through Google Cloud Vertex AI.
//!
//! Vertex hosts Anthropic models at a regional endpoint. The request/response
//! body is the same as Anthropic's Messages API, but requests need a Google
//! OAuth2 access token in the `Authorization: Bearer <token>` header. We reuse
//! `anthropic::build_anthropic_body` and `anthropic::parse_anthropic_stream`.
//!
//! Reference:
//! - https://docs.anthropic.com/en/api/claude-on-google-vertex
//! - https://cloud.google.com/vertex-ai/docs/reference/rest

use crate::adapters::anthropic;
use crate::shared::{Message, ModelInfo, StreamEvent};

use super::ModelAdapter;

/// Vertex AI path for Anthropic models.
///
/// `model_id` is the publisher/model id, e.g. `claude-3-5-sonnet-v2@20241022`.
/// `project_id` and `region` come from `Config::gcp_project_id` and
/// `Config::gcp_region`. Authentication uses a GCP service-account key
/// (configured path or `GOOGLE_APPLICATION_CREDENTIALS`).
pub struct AnthropicVertexAdapter {
    model_id: String,
    project_id: String,
    region: String,
    service_account_path: Option<std::path::PathBuf>,
    client: reqwest::Client,
    json_mode: bool,
    response_format: Option<crate::shared::ResponseFormat>,
    seed: Option<u64>,
    timeout_secs: u64,
    extended_thinking: bool,
    budget_tokens: usize,
    stream_idle_timeout: std::time::Duration,
    /// Cached OAuth token (WO 43.22): fetched once and reused across
    /// stream() calls until near expiry, instead of one roundtrip per
    /// turn. Mutex (not RwLock) so a stampede of concurrent streams
    /// coalesces into a single fetch.
    token_cache: tokio::sync::Mutex<Option<yup_oauth2::AccessToken>>,
}

impl AnthropicVertexAdapter {
    pub fn new(
        model_id: &str,
        project_id: &str,
        region: &str,
        service_account_path: Option<std::path::PathBuf>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            model_id: model_id.to_string(),
            project_id: project_id.to_string(),
            region: region.to_string(),
            service_account_path,
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            extended_thinking: true,
            budget_tokens: 10_000,
            stream_idle_timeout: super::STREAM_IDLE_TIMEOUT,
            token_cache: tokio::sync::Mutex::new(None),
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:streamRawPredict",
            self.region, self.project_id, self.region, self.model_id
        )
    }

    /// Obtain a short-lived access token for the configured service
    /// account, cached until near expiry (WO 43.22).
    /// `AccessToken::is_expired` already applies a 1-minute safety
    /// margin. ceiling: a token whose server sent no expiry is cached
    /// for the adapter's lifetime — GCP always sends expires_in.
    async fn cached_access_token(&self) -> anyhow::Result<String> {
        let mut cache = self.token_cache.lock().await;
        if let Some(t) = cache.as_ref() {
            if !t.is_expired() {
                if let Some(s) = t.token() {
                    return Ok(s.to_string());
                }
            }
        }
        let token = super::vertex_auth::service_account_token(
            self.service_account_path.as_deref(),
            &["https://www.googleapis.com/auth/cloud-platform"],
        )
        .await?;
        let s = token
            .token()
            .ok_or_else(|| anyhow::anyhow!("service account token endpoint returned None"))?
            .to_string();
        *cache = Some(token);
        Ok(s)
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicVertexAdapter {
    fn model_info(&self) -> ModelInfo {
        super::anthropic_model_info(&self.model_id, "claude-3")
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

    fn set_extended_thinking(&mut self, enabled: bool) {
        self.extended_thinking = enabled;
    }

    fn set_budget_tokens(&mut self, budget: usize) {
        self.budget_tokens = budget;
    }

    fn set_streaming_timeout(&mut self, secs: u64) {
        self.stream_idle_timeout = std::time::Duration::from_secs(secs);
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = anthropic::build_anthropic_body(
            &self.model_id,
            messages,
            tools,
            self.response_format.as_ref(),
            self.seed,
            self.extended_thinking,
            self.budget_tokens,
            8192,
            None,
        );
        let url = self.endpoint();
        let token = self.cached_access_token().await?;

        let response = super::send_with_retry(|| async {
            self.client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
        })
        .await?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        tokio::spawn(anthropic::parse_anthropic_stream(
            tx,
            response.bytes_stream(),
            self.stream_idle_timeout,
        ));
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolCallStyle;

    #[test]
    fn endpoint_includes_project_region_and_model() {
        let a = AnthropicVertexAdapter::new(
            "claude-3-5-sonnet-v2",
            "my-project",
            "us-central1",
            None,
            30,
        );
        assert!(a
            .endpoint()
            .contains("us-central1-aiplatform.googleapis.com"));
        assert!(a.endpoint().contains("projects/my-project"));
        assert!(a.endpoint().contains("locations/us-central1"));
        assert!(a.endpoint().contains("claude-3-5-sonnet-v2"));
    }

    #[test]
    fn model_info_reports_image_support_for_claude3() {
        let a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_images);
    }

    #[test]
    fn endpoint_includes_stream_raw_predict_suffix() {
        let a = AnthropicVertexAdapter::new("claude-3-5-sonnet", "p", "us-central1", None, 30);
        assert!(a.endpoint().ends_with(":streamRawPredict"));
    }

    #[test]
    fn endpoint_includes_publishers_anthropic() {
        let a = AnthropicVertexAdapter::new("claude-3-5-sonnet", "p", "us-central1", None, 30);
        assert!(a.endpoint().contains("publishers/anthropic"));
    }

    #[test]
    fn endpoint_for_europe_west_region() {
        let a = AnthropicVertexAdapter::new("claude-3-opus", "p", "europe-west4", None, 30);
        assert!(a
            .endpoint()
            .contains("europe-west4-aiplatform.googleapis.com"));
    }

    #[test]
    fn model_info_reasoning_for_claude_3_7() {
        let a = AnthropicVertexAdapter::new("claude-3-7-sonnet", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_reasoning_for_claude_4() {
        let a = AnthropicVertexAdapter::new("claude-4-opus", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_3_5() {
        let a = AnthropicVertexAdapter::new("claude-3-5-sonnet", "p", "us-central1", None, 30);
        assert!(!a.model_info().supports_thinking);
    }

    // WO 45.62: current-shipping model families via Vertex.
    #[test]
    fn model_info_reasoning_for_claude_sonnet_5() {
        let a = AnthropicVertexAdapter::new("claude-sonnet-5", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_reasoning_for_claude_opus_4_8() {
        let a = AnthropicVertexAdapter::new("claude-opus-4-8", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_haiku_4_5() {
        let a = AnthropicVertexAdapter::new("claude-haiku-4-5", "p", "us-central1", None, 30);
        assert!(!a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_images_for_claude_4() {
        let a = AnthropicVertexAdapter::new("claude-4-opus", "p", "us-central1", None, 30);
        assert!(!a.model_info().supports_images);
    }

    #[test]
    fn model_info_anthropic_tool_call_format() {
        let a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert_eq!(a.model_info().tool_call_format, ToolCallStyle::Anthropic);
    }

    #[test]
    fn model_info_supports_cache() {
        let a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn model_info_max_context_tokens() {
        let a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert_eq!(a.model_info().max_context_tokens, 200_000);
    }

    #[test]
    fn set_json_mode_toggles_flag() {
        let mut a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
    }

    #[test]
    fn set_seed_sets_value() {
        let mut a = AnthropicVertexAdapter::new("claude-3-opus", "p", "us-central1", None, 30);
        assert!(a.seed.is_none());
        a.set_seed(Some(55));
        assert_eq!(a.seed, Some(55));
    }

    #[test]
    fn new_stores_service_account_path() {
        let path = std::path::PathBuf::from("/tmp/key.json");
        let a = AnthropicVertexAdapter::new(
            "claude-3-opus",
            "p",
            "us-central1",
            Some(path.clone()),
            30,
        );
        assert_eq!(a.service_account_path, Some(path));
    }
}
