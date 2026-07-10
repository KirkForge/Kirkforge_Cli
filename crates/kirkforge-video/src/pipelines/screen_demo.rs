//! `screen_demo` — recorded-clips walkthrough pipeline.
//!
//! ponytail: software-demo pipeline shape is the opposite of the explainer.
//! Real video is the star, captions are chrome. Three stages:
//!   1. ScenePlan: scan `<project>/clips/*.mp4` in lexical order, write
//!      a scene_plan.json with one `clip_cut` per clip and an `end_tag`.
//!   2. Assets: leave a manifest (no transcoding yet — clips come in as
//!      recorded and ffmpeg normalizes them at render).
//!   3. Compose: synthesize a `composition.json` from the plan with a
//!      silent bed, ready for `kf render final.mp4`.
//!
//! DecisionLog: skipped for the smallest valid pipeline; the explainer
//! pipeline is the one that needs audit-trail-heavy stages. Add later
//! when the orchestrator surface asks for it.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;

use crate::compose::{AudioSpec, Composition, Scene, TerminalStep};
use crate::error::{KfError, Result};
use crate::orchestrator::Stage;
use crate::pipelines::Pipeline;
use crate::tools::ToolRegistry;

pub struct ScreenDemo;

#[async_trait]
impl Pipeline for ScreenDemo {
    fn name(&self) -> &'static str {
        "screen_demo"
    }
    fn description(&self) -> &'static str {
        "Recorded-clip walkthrough. Drops MP4s into `<project>/clips/`, gets a captioned screencast."
    }
    fn stages(&self) -> &'static [Stage] {
        &[Stage::ScenePlan, Stage::Assets, Stage::Compose]
    }
    async fn run_stage(&self, stage: Stage, dir: &Path, _reg: &ToolRegistry) -> Result<String> {
        let arts = dir.join("artifacts");
        std::fs::create_dir_all(&arts)?;
        match stage {
            Stage::ScenePlan => write_scene_plan(dir, &arts).await,
            Stage::Assets => write_asset_manifest(dir, &arts).await,
            Stage::Compose => write_composition(&arts).await,
            other => Err(KfError::Artifact(format!(
                "screen_demo: stage {other:?} not part of this pipeline"
            ))),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ProjectClips {
    /// Optional hand-authored override list. Each entry is
    /// `<file>:<in_s>:<out_s>` or `<file>` (whole clip).
    /// ponytail: `clip_overrides.json` is ignored if missing.
    overrides: Vec<String>,
}

async fn write_scene_plan(dir: &Path, arts: &Path) -> Result<String> {
    // 1. Collect all `.mp4` files from `<project>/clips/` in sorted order.
    //    Missing dir → empty plan with a single HeroTitle so the pipeline
    //    still produces a valid composition (so `kf doctor` passes).
    let clips_dir = dir.join("clips");
    let mut entries: Vec<PathBuf> = if clips_dir.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&clips_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "mp4").unwrap_or(false))
            .collect();
        v.sort();
        v
    } else {
        Vec::new()
    };
    // ponytail: optional `clip_overrides.json` may inject explicit
    // (file, in_s, out_s) entries. Hand-authored scripts use this to
    // trim a 10-minute screen recording down to the interesting 90s.
    let overrides_path = dir.join("clip_overrides.json");
    if overrides_path.exists() {
        let raw = std::fs::read_to_string(&overrides_path)?;
        let ov: ProjectClips = serde_json::from_str(&raw)
            .map_err(|e| KfError::Artifact(format!("clip_overrides.json: {e}")))?;
        let parsed: Vec<(PathBuf, f32, f32)> = ov
            .overrides
            .iter()
            .filter_map(|s| {
                let parts: Vec<&str> = s.split(':').collect();
                match parts.as_slice() {
                    [f] => Some((clips_dir.join(f), 0.0, 0.0)),
                    [f, a, b] => Some((clips_dir.join(f), a.parse().ok()?, b.parse().ok()?)),
                    _ => None,
                }
            })
            .collect();
        if !parsed.is_empty() {
            entries = parsed.into_iter().map(|(p, _, _)| p).collect();
            // Re-parse to capture (in, out) — refactor for the inline case:
            // ponytail: simplest is to rebuild the entries list once more.
            let triples: Vec<(PathBuf, f32, f32)> = ov
                .overrides
                .iter()
                .filter_map(|s| {
                    let parts: Vec<&str> = s.split(':').collect();
                    match parts.as_slice() {
                        [f] => Some((clips_dir.join(f), 0.0, 0.0)),
                        [f, a, b] => Some((clips_dir.join(f), a.parse().ok()?, b.parse().ok()?)),
                        _ => None,
                    }
                })
                .collect();
            entries.clear();
            for (p, _, _) in &triples {
                entries.push(p.clone());
            }
            // Stash for the Assets stage via a side-channel write:
            std::fs::write(
                arts.join("clip_ranges.json"),
                serde_json::to_string(&triples)?,
            )?;
        }
    }
    // 2. Build the plan. Empty clips dir → hero title + end_tag.
    let plan_path = arts.join("scene_plan.json");
    let mut scenes: Vec<serde_json::Value> = Vec::new();
    if entries.is_empty() {
        scenes.push(serde_json::json!({
            "type": "hero_title",
            "title": "screen demo",
            "subtitle": "drop .mp4 files into clips/ then re-run",
            "duration_s": 3.0,
        }));
    } else {
        for path in &entries {
            // ponytail: duration is approximate. ffprobe would be exact;
            // the Compose stage writes in_s/out_s and ffmpeg trims at
            // render time, so authoring cost here is intentionally low.
            // For overrides, in_s/out_s come from the JSON.
            let (in_s, out_s) = if arts.join("clip_ranges.json").exists() {
                let raw = std::fs::read_to_string(arts.join("clip_ranges.json"))?;
                let triples: Vec<(PathBuf, f32, f32)> = serde_json::from_str(&raw)?;
                triples
                    .iter()
                    .find(|(p, _, _)| p == path)
                    .map(|(_, i, o)| (*i, *o))
                    .unwrap_or((0.0, 6.0))
            } else {
                (0.0, 6.0)
            };
            let dur = (out_s - in_s).max(0.1);
            scenes.push(serde_json::json!({
                "type": "clip_cut",
                "src": path.to_string_lossy(),
                "in_s": in_s,
                "out_s": out_s,
                "duration_s": dur,
            }));
        }
    }
    scenes.push(serde_json::json!({
        "type": "end_tag",
        "title": "demo.kirkforge.video",
        "duration_s": 2.0,
    }));
    let plan = serde_json::json!({
        "kind": "scene_plan",
        "scenes": scenes,
    });
    std::fs::write(&plan_path, serde_json::to_string_pretty(&plan)?)?;
    Ok(plan_path.to_string_lossy().into_owned())
}

async fn write_asset_manifest(dir: &Path, arts: &Path) -> Result<String> {
    // ponytail: nothing to transcode yet. Compose trusts the source
    // clips directly. Add a transcoder pass here when a real screen_demo
    // job fails the first render.
    let manifest = serde_json::json!({
        "kind": "asset_manifest",
        "clips": [],
        "transcode_map": {},
    });
    // ponytail: list whatever happens to exist in clips/, so a downstream
    // tool can introspect without re-reading the directory.
    let clips_dir = dir.join("clips");
    if clips_dir.is_dir() {
        let _ = std::fs::write(
            arts.join("asset_manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "kind": "asset_manifest",
                "clips": std::fs::read_dir(&clips_dir)?.filter_map(|e| e.ok())
                    .map(|e| e.path().to_string_lossy().into_owned())
                    .filter(|s| s.ends_with(".mp4"))
                    .collect::<Vec<_>>(),
                "transcode_map": {},
            }))?,
        );
    } else {
        std::fs::write(
            arts.join("asset_manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;
    }
    Ok(arts
        .join("asset_manifest.json")
        .to_string_lossy()
        .into_owned())
}

async fn write_composition(arts: &Path) -> Result<String> {
    // ponytail: composition.json is built directly from scene_plan.json
    // using `kf validate` semantics — drift between ScreenDemo and
    // AnimatedExplainer's plan shapes is one source of bugs. Reuse the
    // public synthesizer (the render path uses the same one).
    let plan_path = arts.join("scene_plan.json");
    if !plan_path.exists() {
        return Err(KfError::Artifact(
            "scene_plan.json missing — run ScenePlan first".into(),
        ));
    }
    let raw = std::fs::read_to_string(&plan_path)?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    // ponytail: synthesize_from_plan sets audio=Silent. For a screen
    // demo we want a quiet bed so the demo doesn't feel dead. Replace
    // with a low-frequency tone to keep the timeline audible.
    let mut comp: Composition =
        crate::synthesize_from_plan(&v).map_err(|e| KfError::Artifact(format!("compose: {e}")))?;
    // ponytail: if the plan contains any ClipCut scenes, set audio to a
    // soft 80Hz bed. Otherwise stay silent (the empty-plan hero path).
    let has_clips = comp
        .scenes
        .iter()
        .any(|s| matches!(s, Scene::ClipCut { .. }));
    if has_clips {
        comp.audio = Some(AudioSpec::Tone { freq_hz: 80 });
    }
    let comp_path = arts.join("composition.json");
    std::fs::write(&comp_path, serde_json::to_string_pretty(&comp)?)?;
    // ponytail: screen-demo pipelines also benefit from a terminal-style
    // "ingest" pill at scene 0, since the user often drops in arbitrary
    // filenames. Two-line reminder, ~2.5s. Skip when the plan already
    // has it (idempotent re-runs).
    if matches!(comp.scenes.first(), Some(Scene::TerminalScene { .. })) {
        return Ok(comp_path.to_string_lossy().into_owned());
    }
    let mut scenes: Vec<Scene> = Vec::with_capacity(comp.scenes.len() + 1);
    scenes.push(Scene::TerminalScene {
        title: Some("ingest".into()),
        prompt: "$ ".into(),
        accent_color: None,
        steps: vec![
            TerminalStep::Out {
                text: format!(
                    "loaded {} clip(s) from clips/",
                    comp.scenes
                        .iter()
                        .filter(|s| matches!(s, Scene::ClipCut { .. }))
                        .count()
                ),
                hold_s: 1.0,
            },
            TerminalStep::Pill {
                text: "READY".into(),
                color: Some("#27c93f".into()),
                hold_s: 1.5,
            },
        ],
        duration_s: 2.5,
        shot: None,
    });
    scenes.extend(comp.scenes.into_iter());
    comp.scenes = scenes;
    std::fs::write(&comp_path, serde_json::to_string_pretty(&comp)?)?;
    Ok(comp_path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::Stage;
    use crate::tools::ToolRegistry;

    #[tokio::test]
    async fn screen_demo_runs_three_stages_on_empty_project() {
        // ponytail: with no `clips/` dir, the pipeline should produce
        // a valid (synthetic) plan + composition. No I/O outside the
        // tempfile.
        let dir = tempdir();
        let reg = ToolRegistry::default();
        for stage in [Stage::ScenePlan, Stage::Assets, Stage::Compose] {
            let p = ScreenDemo
                .run_stage(stage, &dir, &reg)
                .await
                .expect("stage should succeed");
            assert!(
                std::path::Path::new(&p).exists(),
                "{stage:?} should write its artifact"
            );
        }
        let comp_raw = std::fs::read_to_string(dir.join("artifacts/composition.json"))
            .expect("composition.json present");
        let comp: Composition = serde_json::from_str(&comp_raw).unwrap();
        // Empty plan → terminal ingest + hero_title + end_tag.
        assert!(comp
            .scenes
            .iter()
            .any(|s| matches!(s, Scene::TerminalScene { .. })));
        assert!(comp
            .scenes
            .iter()
            .any(|s| matches!(s, Scene::HeroTitle { .. })));
    }

    #[tokio::test]
    async fn screen_demo_handles_clip_overrides() {
        // ponytail: `clip_overrides.json` lets users hand-pick ranges
        // from inside a long screen recording. The pipeline should
        // read it, drop the corresponding plan entries, and the
        // resulting composition.json should reflect at least one
        // clip_cut scene.
        let dir = tempdir();
        let clips = dir.join("clips");
        std::fs::create_dir_all(&clips).unwrap();
        std::fs::write(clips.join("a.mp4"), b"fake mp4 bytes").unwrap();
        std::fs::write(
            dir.join("clip_overrides.json"),
            r#"{
            "overrides": ["a.mp4:1.0:4.5"]
        }"#,
        )
        .unwrap();
        let reg = ToolRegistry::default();
        ScreenDemo
            .run_stage(Stage::ScenePlan, &dir, &reg)
            .await
            .unwrap();
        let plan_raw = std::fs::read_to_string(dir.join("artifacts/scene_plan.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&plan_raw).unwrap();
        let clips = v["scenes"].as_array().unwrap();
        assert!(
            clips.iter().any(|s| s["type"] == "clip_cut"
                && s["in_s"].as_f64() == Some(1.0)
                && s["out_s"].as_f64() == Some(4.5)),
            "expected clip_cut with override times, got: {plan_raw}"
        );
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "kf-screen-demo-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
