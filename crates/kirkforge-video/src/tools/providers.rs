//! Provider tool stubs — match OpenMontage's catalog (Veo, Runway, ElevenLabs,
//! Suno, etc.) without calling real APIs. Each stub fails with a clear,
//! actionable error so the orchestrator can surface it without crashing.
//!
//! ponytail: this exists so the registry advertises the same surface as the
//! upstream tool catalog. Replace `invoke` bodies when an API key is wired in.

use std::path::Path;

use async_trait::async_trait;

use crate::error::{KfError, Result};
use crate::tools::{Tool, ToolOutput, ToolStability, ToolTier};

/// ponytail: each provider reads `<NAME>_API_KEY` from the env. If absent,
/// the stub returns a structured error pointing at the env var to set.
fn missing_key(provider: &str) -> KfError {
    let env = format!("{}_API_KEY", provider.to_uppercase());
    KfError::Artifact(format!(
        "{provider} provider not configured: set ${env} to enable"
    ))
}

macro_rules! provider_stub {
    ($name:literal, $capabilities:expr, $provider:expr) => {
        // ponytail: one struct per provider, lowercase name = snake_case tool id
        // as registered. UpperCamelCase struct name keeps rustc happy.
        paste::paste! {
            pub struct [<$name:camel>];
            impl [<$name:camel>] { pub fn new() -> Self { Self } }
            impl Default for [<$name:camel>] { fn default() -> Self { Self::new() } }

            #[async_trait]
            impl Tool for [<$name:camel>] {
                fn name(&self) -> &'static str { $name }
                fn tier(&self) -> ToolTier { ToolTier::Provider }
                fn stability(&self) -> ToolStability { ToolStability::Experimental }
                fn capabilities(&self) -> &'static [&'static str] { &$capabilities }

                async fn invoke(&self, _project: &Path, _op: &str, _params: serde_json::Value) -> Result<ToolOutput> {
                    Err(missing_key($provider))
                }
            }
        }
    };
}

provider_stub!(
    "veo",
    ["text_to_video", "image_to_video", "extend_clip"],
    "veo"
);
provider_stub!(
    "runway",
    ["text_to_video", "image_to_video", "act_two"],
    "runway"
);
provider_stub!("elevenlabs", ["tts", "voice_clone", "dub"], "elevenlabs");
provider_stub!("suno", ["music_generate", "lyrics_generate"], "suno");
provider_stub!(
    "openai_image",
    ["text_to_image", "edit", "variation"],
    "openai_image"
);
