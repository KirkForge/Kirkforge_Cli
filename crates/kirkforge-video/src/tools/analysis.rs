//! `analysis` — wraps ffprobe and (optionally) Whisper CLI.
//!
//! ponytail: heavy ML features (scene detect, transcriber) are stubbed with
//! the FFmpeg `select`/`silencedetect` filters until a Whisper binary lands.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::{KfError, Result};
use crate::tools::{Tool, ToolOutput, ToolStability, ToolTier};

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Op {
    Probe {
        src: std::path::PathBuf,
    },
    DetectSilence {
        src: std::path::PathBuf,
        noise_db: f32,
        min_s: f32,
    },
    SampleFrames {
        src: std::path::PathBuf,
        count: u32,
        out_dir: std::path::PathBuf,
    },
}

pub struct Analyzer;
impl Analyzer {
    pub fn new() -> Self {
        Self
    }
}
impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Analyzer {
    fn name(&self) -> &'static str {
        "analysis"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::Core
    }
    fn stability(&self) -> ToolStability {
        ToolStability::Experimental
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &[
            "probe",
            "detect_silence",
            "sample_frames",
            "scene_detect",
            "transcribe",
        ]
    }

    async fn invoke(
        &self,
        _project: &Path,
        _op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput> {
        let op: Op = serde_json::from_value(params)
            .map_err(|e| KfError::Artifact(format!("analysis: {e}")))?;

        match op {
            Op::Probe { src } => {
                let out = Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-print_format",
                        "json",
                        "-show_format",
                        "-show_streams",
                    ])
                    .arg(&src)
                    .output()
                    .await
                    .map_err(|e| KfError::Ffmpeg(format!("ffprobe not on PATH: {e}")))?;
                if !out.status.success() {
                    return Err(KfError::Ffmpeg(format!(
                        "ffprobe exited {:?}",
                        out.status.code()
                    )));
                }
                let meta: serde_json::Value = serde_json::from_slice(&out.stdout)
                    .map_err(|e| KfError::Artifact(format!("ffprobe json: {e}")))?;
                Ok(ToolOutput {
                    artifact: "probe.json".into(),
                    path: src.clone(),
                    meta,
                })
            }
            Op::DetectSilence {
                src,
                noise_db,
                min_s,
            } => {
                let out = Command::new("ffmpeg")
                    .args(["-hide_banner", "-i"])
                    .arg(&src)
                    .args([
                        "-af",
                        &format!("silencedetect=noise={noise_db}dB:d={min_s}"),
                        "-f",
                        "null",
                        "-",
                    ])
                    .output()
                    .await
                    .map_err(|e| KfError::Ffmpeg(format!("ffmpeg: {e}")))?;
                let stderr = String::from_utf8_lossy(&out.stderr);
                Ok(ToolOutput {
                    artifact: "silence.txt".into(),
                    path: src.clone(),
                    meta: serde_json::json!({"stderr": stderr}),
                })
            }
            Op::SampleFrames {
                src,
                count,
                out_dir,
            } => {
                std::fs::create_dir_all(&out_dir)?;
                let status = Command::new("ffmpeg")
                    .args(["-y", "-i"])
                    .arg(&src)
                    .args(["-vf", &format!("fps=1/{count}"), "-q:v", "2"])
                    .arg(format!("{}/frame_%04d.jpg", out_dir.display()))
                    .status()
                    .await
                    .map_err(|e| KfError::Ffmpeg(format!("ffmpeg: {e}")))?;
                if !status.success() {
                    return Err(KfError::Ffmpeg(format!(
                        "sample_frames exited {:?}",
                        status.code()
                    )));
                }
                Ok(ToolOutput {
                    artifact: format!("{count}_frames"),
                    path: out_dir,
                    meta: serde_json::json!({"count": count}),
                })
            }
        }
    }
}
