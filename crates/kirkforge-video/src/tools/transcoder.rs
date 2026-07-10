//! `transcoder` — normalize clips to h264/yuv420p so the FFmpeg filter
//! graph's concat + drawtext chain never blows up on a webm/prores/mov
//! input.
//!
//! ponytail: ffmpeg `-c:v libx264 -pix_fmt yuv420p` covers the 90% case.
//! HDR / 10-bit / VP9 / ProRes would each want a different path — add
//! profiles when a real pipeline needs them.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::{KfError, Result};
use crate::tools::{Tool, ToolOutput, ToolStability, ToolTier};

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Op {
    /// ponytail: `src` → `dst` in the same video size + framerate, normalized
    /// to h264 yuv420p. `crf` (0..=51, default 23) tunes quality.
    Transcode {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
        #[serde(default = "default_crf")]
        crf: u32,
    },
}

fn default_crf() -> u32 {
    23
}

pub struct Transcoder;
impl Transcoder {
    pub fn new() -> Self {
        Self
    }
}
impl Default for Transcoder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Transcoder {
    fn name(&self) -> &'static str {
        "transcoder"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::Core
    }
    fn stability(&self) -> ToolStability {
        ToolStability::Stable
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &["transcode"]
    }

    async fn invoke(
        &self,
        _project: &Path,
        _op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput> {
        let op: Op = serde_json::from_value(params)
            .map_err(|e| KfError::Artifact(format!("transcoder: {e}")))?;
        match op {
            Op::Transcode { src, dst, crf } => {
                if let Some(parent) = dst.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
                let out = Command::new("ffmpeg")
                    .args(["-y", "-i"])
                    .arg(&src)
                    .args([
                        "-c:v",
                        "libx264",
                        "-pix_fmt",
                        "yuv420p",
                        "-crf",
                        &crf.to_string(),
                        "-c:a",
                        "aac",
                        "-b:a",
                        "128k",
                        "-movflags",
                        "+faststart",
                    ])
                    .arg(&dst)
                    .output()
                    .await
                    .map_err(|e| KfError::Ffmpeg(format!("ffmpeg: {e}")))?;
                if !out.status.success() {
                    return Err(KfError::Ffmpeg(format!(
                        "transcode exited {:?}\nstderr:\n{}",
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr),
                    )));
                }
                Ok(ToolOutput {
                    artifact: "transcoded.mp4".into(),
                    path: dst,
                    meta: serde_json::json!({
                        "src": src.to_string_lossy(),
                        "crf": crf,
                    }),
                })
            }
        }
    }
}
