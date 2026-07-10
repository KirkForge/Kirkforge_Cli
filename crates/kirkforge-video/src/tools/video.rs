//! `video_stitch` — FFmpeg-backed multi-clip assembly.
//!
//! Capabilities: validate_clips, stitch, crossfade, fade_through_black,
//! preview_stitch, spatial_side_by_side, spatial_vertical_stack,
//! spatial_picture_in_picture.
//!
//! ponytail: filter graph assembled inline with `format!`; switch to a typed
//! builder if the graph ever exceeds ~10 chained filters.

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
    Stitch {
        clips: Vec<PathBuf>,
        out: PathBuf,
    },
    Crossfade {
        clips: Vec<PathBuf>,
        duration_s: f32,
        out: PathBuf,
    },
    FadeThroughBlack {
        clips: Vec<PathBuf>,
        duration_s: f32,
        out: PathBuf,
    },
    SideBySide {
        left: PathBuf,
        right: PathBuf,
        out: PathBuf,
    },
    VerticalStack {
        top: PathBuf,
        bottom: PathBuf,
        out: PathBuf,
    },
    PictureInPicture {
        bg: PathBuf,
        fg: PathBuf,
        x: i32,
        y: i32,
        scale: f32,
        out: PathBuf,
    },
}

pub struct VideoStitch;

impl VideoStitch {
    pub fn new() -> Self {
        Self
    }
}

impl Default for VideoStitch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for VideoStitch {
    fn name(&self) -> &'static str {
        "video_stitch"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::Core
    }
    fn stability(&self) -> ToolStability {
        ToolStability::Beta
    }
    fn capabilities(&self) -> &'static [&'static str] {
        &[
            "validate_clips",
            "stitch",
            "crossfade",
            "fade_through_black",
            "preview_stitch",
            "spatial_side_by_side",
            "spatial_vertical_stack",
            "spatial_picture_in_picture",
        ]
    }

    async fn invoke(
        &self,
        _project: &Path,
        _op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput> {
        let op: Op = serde_json::from_value(params)
            .map_err(|e| KfError::Artifact(format!("video_stitch: {e}")))?;
        let (args, out) = match op {
            Op::Stitch { clips, out } => {
                let mut a = vec!["-y".to_string()];
                for c in &clips {
                    a.extend(["-i".into(), c.display().to_string()]);
                }
                let n = clips.len();
                let filter = (0..n)
                    .map(|i| format!("[{i}:v][{i}:a]"))
                    .collect::<String>()
                    + &format!("concat=n={n}:v=1:a=1[v][a]");
                a.extend([
                    "-filter_complex".into(),
                    filter,
                    "-map".into(),
                    "[v]".into(),
                    "-map".into(),
                    "[a]".into(),
                    out.display().to_string(),
                ]);
                (a, out)
            }
            Op::Crossfade {
                clips,
                duration_s,
                out,
            } => {
                if clips.len() < 2 {
                    return Err(KfError::Artifact("crossfade needs ≥2 clips".into()));
                }
                let n = clips.len();
                let mut inputs = vec!["-y".to_string()];
                for c in &clips {
                    inputs.extend(["-i".into(), c.display().to_string()]);
                }
                // Chain xfade between consecutive streams.
                let mut filter = String::new();
                let mut last = "0:v".to_string();
                let mut last_a = "0:a".to_string();
                for i in 1..n {
                    let next_v = format!("{i}:v");
                    let next_a = format!("{i}:a");
                    let out_v = if i == n - 1 {
                        "vout".to_string()
                    } else {
                        format!("v{i}")
                    };
                    let out_a = if i == n - 1 {
                        "aout".to_string()
                    } else {
                        format!("a{i}")
                    };
                    filter.push_str(&format!(
                        "[{last}][{next_v}]xfade=transition=fade:duration={duration_s}[{out_v}];"
                    ));
                    // Audio crossfade via acrossfade.
                    filter.push_str(&format!(
                        "[{last_a}][{next_a}]acrossfade=d={duration_s}[{out_a}];"
                    ));
                    last = out_v;
                    last_a = out_a;
                }
                let mut args = inputs;
                args.extend([
                    "-filter_complex".into(),
                    filter,
                    "-map".into(),
                    "[vout]".into(),
                    "-map".into(),
                    "[aout]".into(),
                    out.display().to_string(),
                ]);
                (args, out)
            }
            Op::FadeThroughBlack {
                clips,
                duration_s,
                out,
            } => {
                if clips.len() < 2 {
                    return Err(KfError::Artifact("fade needs ≥2 clips".into()));
                }
                let n = clips.len();
                let mut inputs = vec!["-y".to_string()];
                for c in &clips {
                    inputs.extend(["-i".into(), c.display().to_string()]);
                }
                let mut filter = String::new();
                let mut last_v = "0:v".to_string();
                let mut last_a = "0:a".to_string();
                for i in 1..n {
                    let next_v = format!("{i}:v");
                    let next_a = format!("{i}:a");
                    let out_v = if i == n - 1 {
                        "vout".to_string()
                    } else {
                        format!("v{i}")
                    };
                    let out_a = if i == n - 1 {
                        "aout".to_string()
                    } else {
                        format!("a{i}")
                    };
                    filter.push_str(&format!(
                        "[{last_v}][{next_v}]xfade=transition=fadeblack:duration={duration_s}[{out_v}];"
                    ));
                    filter.push_str(&format!(
                        "[{last_a}][{next_a}]acrossfade=d={duration_s}[{out_a}];"
                    ));
                    last_v = out_v;
                    last_a = out_a;
                }
                let mut args = inputs;
                args.extend([
                    "-filter_complex".into(),
                    filter,
                    "-map".into(),
                    "[vout]".into(),
                    "-map".into(),
                    "[aout]".into(),
                    out.display().to_string(),
                ]);
                (args, out)
            }
            Op::SideBySide { left, right, out } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    left.display().to_string(),
                    "-i".into(),
                    right.display().to_string(),
                    "-filter_complex".into(),
                    "[0:v]setpts=PTS-STARTPTS,scale=960:-2[vl];\
                     [1:v]setpts=PTS-STARTPTS,scale=960:-2[vr];\
                     [vl][vr]hstack=inputs=2[v]"
                        .into(),
                    "-map".into(),
                    "[v]".into(),
                    "-c:v".into(),
                    "libx264".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::VerticalStack { top, bottom, out } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    top.display().to_string(),
                    "-i".into(),
                    bottom.display().to_string(),
                    "-filter_complex".into(),
                    "[0:v]scale=-2:540[vt];[1:v]scale=-2:540[vb];[vt][vb]vstack=inputs=2[v]".into(),
                    "-map".into(),
                    "[v]".into(),
                    "-c:v".into(),
                    "libx264".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
            Op::PictureInPicture {
                bg,
                fg,
                x,
                y,
                scale,
                out,
            } => {
                let args = vec![
                    "-y".into(),
                    "-i".into(),
                    bg.display().to_string(),
                    "-i".into(),
                    fg.display().to_string(),
                    "-filter_complex".into(),
                    format!(
                        "[1:v]scale=iw*{scale}:ih*{scale}[fg];\
                         [0:v][fg]overlay=x={x}:y={y}[v]"
                    ),
                    "-map".into(),
                    "[v]".into(),
                    "-c:v".into(),
                    "libx264".into(),
                    out.display().to_string(),
                ];
                (args, out)
            }
        };

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out_run = Command::new("ffmpeg")
            .args(&arg_refs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| KfError::Ffmpeg(format!("ffmpeg not on PATH: {e}")))?;

        if !out_run.status.success() {
            let stderr = String::from_utf8_lossy(&out_run.stderr);
            return Err(KfError::Ffmpeg(format!(
                "ffmpeg exited {:?}\nstderr:\n{}",
                out_run.status.code(),
                stderr
            )));
        }
        Ok(ToolOutput {
            artifact: out
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "out.mp4".into()),
            path: out,
            meta: serde_json::json!({"tool": "video_stitch"}),
        })
    }
}
