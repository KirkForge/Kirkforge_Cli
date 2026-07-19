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
use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

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
    timeout_secs: u64,
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
            timeout_secs,
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:streamRawPredict",
            self.region, self.project_id, self.region, self.model_id
        )
    }

    /// Obtain a short-lived access token for the configured service account.
    async fn access_token(&self) -> anyhow::Result<String> {
        super::vertex_auth::service_account_token(
            self.service_account_path.as_deref(),
            &["https://www.googleapis.com/auth/cloud-platform"],
        )
        .await
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicVertexAdapter {
    fn model_info(&self) -> ModelInfo {
        let lower = self.model_id.to_lowercase();
        let is_reasoning = lower.contains("claude-3-7-sonnet") || lower.contains("claude-4");
        ModelInfo {
            name: self.model_id.clone(),
            supports_thinking: is_reasoning,
            tool_call_format: ToolCallStyle::Anthropic,
            max_context_tokens: 200_000,
            recommended_temperature: 1.0,
            supports_images: lower.starts_with("claude-3"),
            supports_cache: true,
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = anthropic::build_anthropic_body(&self.model_id, messages, tools, self.json_mode);
        let url = self.endpoint();
        let token = self.access_token().await?;

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
        ));
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
