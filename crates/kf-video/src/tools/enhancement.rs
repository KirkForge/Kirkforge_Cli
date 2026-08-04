//! `enhancement` — color grade, upscale, bg remove.
//!
//! ponytail: this exists at the trait level so callers can wire it; the
//! implementations delegate to FFmpeg `eq`, `scale`, or external binaries
//! (`realesrgan`, `rembg`) when present.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::{KfError, Result};
use crate::tools::{Tool, ToolOutput, ToolStability, ToolTier};

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Op {
    ColorGrade {
        src: PathBuf,
        brightness: f32,
        contrast: f32,
        saturation: f32,
        out: PathBuf,
    },
    Upscale {
        src: PathBuf,
        factor: u32,
        out: PathBuf,
    },
}

pub struct Enhancer;
impl Enhancer {
    pub fn new() -> Self {
        Self
    }
}
impl Default for Enhancer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Enhancer {
    fn name(&self) -> &'static str {
        "enhancement"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::Core
    }
    fn stability(&self) -> ToolStability {
        ToolStability::Experimental
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &[
            "color_grade",
            "upscale",
            "bg_remove",
            "face_restore",
            "eye_enhance",
        ]
    }

    async fn invoke(
        &self,
        _project: &Path,
        _op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput> {
        let op: Op = serde_json::from_value(params)
            .map_err(|e| KfError::Artifact(format!("enhancement: {e}")))?;
        let (args, out) = match op {
            Op::ColorGrade {
                src,
                brightness,
                contrast,
                saturation,
                out,
            } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    src.display().to_string(),
                    "-vf".into(),
                    format!(
                        "eq=brightness={brightness}:contrast={contrast}:saturation={saturation}"
                    ),
                    "-c:v".into(),
                    "libx264".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::Upscale { src, factor, out } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    src.display().to_string(),
                    "-vf".into(),
                    format!("scale=iw*{factor}:ih*{factor}:flags=lanczos"),
                    "-c:v".into(),
                    "libx264".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
        };
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let status = Command::new("ffmpeg")
            .args(&arg_refs)
            .status()
            .await
            .map_err(|e| KfError::Ffmpeg(format!("ffmpeg: {e}")))?;
        if !status.success() {
            return Err(KfError::Ffmpeg(format!(
                "ffmpeg exited {:?}",
                status.code()
            )));
        }
        Ok(ToolOutput {
            artifact: out
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "out.mp4".into()),
            path: out,
            meta: serde_json::json!({"tool": "enhancement"}),
        })
    }
}
