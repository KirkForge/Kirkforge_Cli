# ADR 0022: Anthropic cloud routing — Bedrock and Vertex

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

Anthropic's own API is not the only way to run Claude. AWS Bedrock and Google Cloud Vertex AI both host Anthropic models behind their own authentication and endpoints. Users want to keep the same model behavior (native tool calls, prompt caching, extended thinking) while routing through their existing cloud credentials. We need adapters that speak the same Anthropic Messages API wire format but handle AWS SigV4 signing or GCP service-account tokens.

## Decision

Add two new model adapters:

- `AnthropicBedrockAdapter` in `src/adapters/anthropic_bedrock.rs`
- `AnthropicVertexAdapter` in `src/adapters/anthropic_vertex.rs`

Both reuse the existing `anthropic::build_anthropic_body` body builder and `anthropic::parse_anthropic_stream` parser so the wire-format behavior stays identical to the first-party adapter.

### Selection rules

- A CLI `--model-type` override of `anthropic-bedrock`, `bedrock`, `anthropic-vertex`, or `vertex` forces the corresponding adapter.
- When no override is given and the model name matches Claude, `Config::anthropic_provider` decides: values `bedrock` or `vertex` select the cloud adapter; any other value falls back to the direct Anthropic adapter.
- Bedrock model ids look like `anthropic.claude-3-5-sonnet-20240620-v1:0`; Vertex model ids look like `claude-3-5-sonnet-v2@20241022`.

### AWS Bedrock adapter

- Endpoint: `https://bedrock-runtime.{region}.amazonaws.com/model/{model_id}/invoke-with-response-stream`
- Signs requests with SigV4 using the `aws-sigv4`, `aws-credential-types`, and `aws-smithy-runtime-api` crates.
- Credentials come from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` in this MVP; profile/instance support is an extension point.
- The event-stream response is stripped of its AWS envelope and each JSON payload is fed to the shared Anthropic parser as an SSE `data:` line.

### GCP Vertex adapter

- Endpoint: `https://{region}-aiplatform.googleapis.com/v1/projects/{project_id}/locations/{region}/publishers/anthropic/models/{model_id}:streamRawPredict`
- Authenticates with a GCP service-account JSON key via `yup-oauth2`, using the configured `gcp_service_account_path` or `GOOGLE_APPLICATION_CREDENTIALS`.
- Sends `Authorization: Bearer <token>` and streams the Anthropic response directly.

### Configuration additions

- `anthropic_provider` (string, default `"anthropic"`)
- `aws_region` (string, default `"us-east-1"`)
- `aws_profile` (string, currently a placeholder for the AWS SDK profile chain)
- `gcp_project_id` (string)
- `gcp_region` (string, default `"us-central1"`)
- `gcp_service_account_path` (optional path)

All fields have matching `KIRKFORGE_*` environment variables.

## Consequences

- Users can run Claude through the cloud provider they already have credentials for, without changing the rest of the model interaction.
- The adapter enum grew two variants; all match sites were updated to treat Bedrock and Vertex the same as Anthropic for `/model` switching validation.
- The `aws-sigv4` and `yup-oauth2` dependencies are added only for signing/token retrieval; no full AWS or GCP SDK is required.

## Future work

- Add a real AWS config profile resolver when `aws_profile` is set.
- Support GCP Application Default Credentials (ADC) beyond service-account keys.
- Cache GCP access tokens to avoid fetching a new token on every request.
