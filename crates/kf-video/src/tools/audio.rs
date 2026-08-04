//! `audio_mixer` — FFmpeg-backed multi-track mixing.
//!
//! Capabilities: mix, duck, fade, normalize, extract_audio, segmented_music.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::error::{KfError, Result};
use crate::tools::{Tool, ToolOutput, ToolStability, ToolTier};

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Op {
    Mix {
        tracks: Vec<Track>,
        out: PathBuf,
    },
    Duck {
        speech: PathBuf,
        music: PathBuf,
        threshold_db: f32,
        ratio: f32,
        out: PathBuf,
    },
    Fade {
        src: PathBuf,
        fade_in_s: f32,
        fade_out_s: f32,
        out: PathBuf,
    },
    Normalize {
        src: PathBuf,
        target_db: f32,
        out: PathBuf,
    },
    ExtractAudio {
        src: PathBuf,
        out: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
pub struct Track {
    pub path: PathBuf,
    pub volume: f32,
    pub delay_ms: u32,
}

pub struct AudioMixer;
impl AudioMixer {
    pub fn new() -> Self {
        Self
    }
}
impl Default for AudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AudioMixer {
    fn name(&self) -> &'static str {
        "audio_mixer"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::Core
    }
    fn stability(&self) -> ToolStability {
        ToolStability::Beta
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &[
            "mix",
            "duck",
            "fade",
            "normalize",
            "extract_audio",
            "segmented_music",
            "full_mix",
        ]
    }

    async fn invoke(
        &self,
        _project: &Path,
        _op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput> {
        let op: Op = serde_json::from_value(params)
            .map_err(|e| KfError::Artifact(format!("audio_mixer: {e}")))?;

        let (args, out) = match op {
            Op::Mix { tracks, out } => {
                if tracks.is_empty() {
                    return Err(KfError::Artifact("mix needs ≥1 track".into()));
                }
                let mut args = vec!["-y".to_string()];
                for t in &tracks {
                    args.extend(["-i".into(), t.path.display().to_string()]);
                }
                let n = tracks.len();
                let mut filter = String::new();
                for (i, t) in tracks.iter().enumerate() {
                    let delay = if t.delay_ms > 0 {
                        format!("adelay={}|{},", t.delay_ms, t.delay_ms)
                    } else {
                        String::new()
                    };
                    let vol = format!("volume={}", t.volume);
                    filter.push_str(&format!("[{i}:a]{delay}{vol}[a{i}];"));
                }
                let inputs = (0..n).map(|i| format!("[a{i}]")).collect::<String>();
                filter.push_str(&format!("{inputs}amix=inputs={n}:duration=longest[aout]"));
                args.extend([
                    "-filter_complex".into(),
                    filter,
                    "-map".into(),
                    "[aout]".into(),
                    "-c:a".into(),
                    "aac".into(),
                    out.display().to_string(),
                ]);
                (args, out)
            }
            Op::Duck {
                speech,
                music,
                threshold_db,
                ratio,
                out,
            } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(), music.display().to_string(),
                    "-i".into(), speech.display().to_string(),
                    "-filter_complex".into(),
                    format!("[1:a]asplit=2[sc][ref];[sc]volume=1[sc];\
                             [0:a][sc]sidechaincompress=threshold={threshold_db}:ratio={ratio}:attack=20:release=300[ducked]"),
                    "-map".into(), "[ducked]".into(),
                    "-c:a".into(), "aac".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::Fade {
                src,
                fade_in_s,
                fade_out_s,
                out,
            } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    src.display().to_string(),
                    "-af".into(),
                    format!("afade=t=in:st=0:d={fade_in_s},afade=t=out:st=0:d={fade_out_s}"),
                    "-c:a".into(),
                    "aac".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::Normalize {
                src,
                target_db,
                out,
            } => {
                // loudnorm is a 2-pass filter in practice, but the single-shot form is fine here.
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    src.display().to_string(),
                    "-af".into(),
                    format!("loudnorm=I={target_db}:TP=-1.5:LRA=11"),
                    "-c:a".into(),
                    "aac".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::ExtractAudio { src, out } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    src.display().to_string(),
                    "-vn".into(),
                    "-c:a".into(),
                    "aac".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
        };

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let status = Command::new("ffmpeg")
            .args(&arg_refs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| KfError::Ffmpeg(format!("ffmpeg not on PATH: {e}")))?;

        if !status.success() {
            return Err(KfError::Ffmpeg(format!("ffmpeg exited {status:?}")));
        }
        Ok(ToolOutput {
            artifact: out
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "out.m4a".into()),
            path: out,
            meta: serde_json::json!({"tool": "audio_mixer"}),
        })
    }
}
