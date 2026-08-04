//! Render a `Composition` to an MP4 by shelling out to `ffmpeg`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::compose::{
    build_filter_graph_with_brand, caption_overlay_srt, AudioSpec, BrandTheme, Composition,
};
use crate::error::{KfError, Result};

pub async fn render_composition(comp: &Composition, out: &Path) -> Result<()> {
    // ponytail: derive project dir from `out` ancestry. The CLI lays
    // outputs out as `<project>/render/final.mp4`, so the project root
    // is `out.parent().parent()`. Missing brand.json → defaults. Tests
    // that drop the output anywhere else also get defaults because no
    // brand.json exists in that tree.
    let brand = out
        .parent()
        .and_then(|p| p.parent())
        .map(BrandTheme::from_project)
        .unwrap_or_default();
    let plan =
        build_filter_graph_with_brand(&comp.scenes, comp.width, comp.height, comp.fps, &brand);

    tracing::debug!(filter = %plan.filter_complex, "ffmpeg filter_complex");
    if let Ok(dump) = std::env::var("KF_DUMP_FG") {
        let _ = std::fs::write(&dump, &plan.filter_complex);
    }

    // ponytail: SRT sidecar + muxed subtitle stream. If any scene is a
    // CaptionOverlay, write `<out>.srt` and add it as a mov_text stream.
    // Players (VLC/MPC) can toggle; ffmpeg's `subtitles=` filter could
    // re-burn it but we already use drawtext for the visual lower-third.
    let srt_path: Option<PathBuf> = {
        let srt = caption_overlay_srt(&comp.scenes);
        if srt.is_empty() {
            None
        } else {
            let p = srt_path_for(out);
            std::fs::write(&p, srt)
                .map_err(|e| KfError::Ffmpeg(format!("write srt sidecar {}: {e}", p.display())))?;
            Some(p)
        }
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y");
    for input in &plan.inputs {
        for tok in input.split_whitespace() {
            cmd.arg(tok);
        }
    }
    // ponytail: audio input(s) follow clip inputs. The voice input index is
    // the lavfi/file input right after the last clip. When duck_under is on
    // we add a second lavfi sine for the bed and mix both. -stream_loop -1
    // is implied by `-shortest` so we don't need to lengthen either track.
    let audio_voice_idx = plan.inputs.len() as u32;
    let audio_bg_idx: Option<u32> = match comp.audio.as_ref() {
        Some(AudioSpec::Narration {
            duck_under: true, ..
        }) => Some(audio_voice_idx + 1),
        _ => None,
    };
    if let Some(spec) = comp.audio.as_ref() {
        match spec {
            AudioSpec::Silent => {
                cmd.args([
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=channel_layout=stereo:sample_rate=44100",
                ]);
            }
            AudioSpec::Tone { freq_hz } => {
                cmd.args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={freq_hz}:sample_rate=44100"),
                ]);
            }
            AudioSpec::Narration { path, .. } => {
                cmd.args(["-i", &path.to_string_lossy()]);
            }
        }
        if audio_bg_idx.is_some() {
            // ponytail: bind the bg sine to the total video length via
            // `-t` so it has a finite EOF on the input. Without `-t`,
            // `sine` is an infinite source and the amix chain's internal
            // sample buffer grows without bound (failing with ENOSPC).
            let total = comp.total_duration_s();
            cmd.args([
                "-f",
                "lavfi",
                "-t",
                &format!("{total}"),
                "-i",
                "sine=frequency=220:sample_rate=44100",
            ]);
        }
    }
    // ponytail: SRT `-i` declared alongside the other inputs so
    // `-filter_complex` and `-map` see it as part of input N.
    if let Some(p) = &srt_path {
        cmd.args(["-i", &p.to_string_lossy()]);
    }
    // ponytail: build the audio chain. The voice is looped via `aloop` so
    // a short narration fills a longer video; the bg sine already has a
    // finite length from `-t`. Both have a known EOF inside the filter
    // graph, so amix terminates correctly.
    let total = comp.total_duration_s();
    let audio_chain = match (comp.audio.as_ref(), audio_bg_idx) {
        (Some(_), Some(bg)) => format!(
            ";[{audio_voice_idx}:a]aresample=44100,aloop=loop=-1:size=2e9,atrim=0:{total}[a_voice];\
             [{bg}:a]aresample=44100,volume=0.2[a_bg];\
             [a_voice][a_bg]amix=inputs=2:duration=first:dropout_transition=0[aout]"
        ),
        (Some(_), None) => format!(
            ";[{audio_voice_idx}:a]aresample=44100,aloop=loop=-1:size=2e9,atrim=0:{total}[aout]"
        ),
        (None, _) => String::new(),
    };
    let full_filter = format!("{}{}", plan.filter_complex, audio_chain);
    cmd.args(["-filter_complex", &full_filter]);
    cmd.args(["-map", "[vout]"]);
    if comp.audio.is_some() {
        cmd.args(["-map", "[aout]"]);
    }
    if srt_path.is_some() {
        let audio_count = match comp.audio.as_ref() {
            Some(AudioSpec::Narration {
                duck_under: true, ..
            }) => 2,
            Some(_) => 1,
            None => 0,
        };
        let srt_input_idx = plan.inputs.len() as u32 + audio_count;
        cmd.args(["-map", &format!("{srt_input_idx}:s")]);
        cmd.args(["-c:s", "mov_text"]);
    }
    cmd.args([
        "-r",
        &comp.fps.to_string(),
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
    ]);
    cmd.args(["-movflags", "+faststart"]);
    cmd.arg(out);

    let out_run = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| KfError::Ffmpeg(format!("ffmpeg: {e}")))?;

    if !out_run.status.success() {
        let stderr = String::from_utf8_lossy(&out_run.stderr);
        return Err(KfError::Ffmpeg(format!(
            "ffmpeg exited {:?}\nstderr:\n{}",
            out_run.status.code(),
            stderr
        )));
    }
    Ok(())
}

/// Build the sidecar path: `out.mp4` -> `out.srt` in the same dir.
fn srt_path_for(out: &Path) -> PathBuf {
    let mut p = out.to_path_buf();
    p.set_extension("srt");
    p
}
