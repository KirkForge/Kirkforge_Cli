//! End-to-end pipeline test.
//!
//! ponytail: spins up a tempfile project, runs `animated_explainer`, asserts
//! the artifact contract + final MP4 invariants. This is the single check
//! that catches regressions in the FFmpeg filter graph, scene synthesis,
//! checkpoint save, and decision log.

use std::path::Path;
use std::process::Command;

use kirkforge_video::compose::Composition;
use kirkforge_video::orchestrator::{Checkpoint, Stage};
use kirkforge_video::pipelines::{AnimatedExplainer, Pipeline};
use kirkforge_video::tools::ToolRegistry;

fn ffprobe_json(path: &Path) -> serde_json::Value {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    serde_json::from_slice(&out.stdout).expect("ffprobe JSON")
}

#[tokio::test]
async fn animated_explainer_renders_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();

    // Seed a brief so the ScenePlan stage produces content beyond stock demo.
    std::fs::write(
        dir_path.join("brief.txt"),
        "Test Pipeline\nIntegration check\n- 7 stages\n- 100% resumable\n- 0 JS at render\n> test.tag\n",
    ).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline");

    // 1. All 8 stage artifacts present.
    let arts = dir_path.join("artifacts");
    for name in [
        "research_brief.json",
        "proposal_packet.json",
        "script.json",
        "scene_plan.json",
        "asset_manifest.json",
        "edit_decisions.json",
        "composition.json",
    ] {
        assert!(arts.join(name).exists(), "missing artifact: {name}");
    }
    assert!(
        arts.join("risk_report.json").exists(),
        "missing risk_report.json"
    );
    assert!(
        arts.join("decision_log.jsonl").exists(),
        "missing decision_log.jsonl"
    );
    // Narration stage artifacts: narration.mp3 is only present when script.json
    // had a non-empty narration (the brief seeds one).
    assert!(
        arts.join("narration.mp3").exists(),
        "missing narration.mp3 — brief seeded a narration line"
    );

    // 2. Composition parses + has at least 4 scenes.
    let comp: Composition =
        serde_json::from_str(&std::fs::read_to_string(arts.join("composition.json")).unwrap())
            .expect("composition parses");
    assert!(
        comp.scenes.len() >= 4,
        "scene count = {}",
        comp.scenes.len()
    );
    // ponytail: when narration.mp3 exists, Compose swaps audio to Narration
    // spec so the rendered MP4 carries the voice track.
    match &comp.audio {
        Some(kirkforge_video::compose::AudioSpec::Narration { path, .. }) => {
            assert!(
                path.exists(),
                "narration path doesn't exist: {}",
                path.display()
            );
        }
        other => panic!("expected Narration audio, got {other:?}"),
    }

    // 3. Checkpoint written with all 8 stages complete.
    let cp = Checkpoint::load_or_init(&dir_path, pipe.name()).expect("checkpoint");
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::Narration,
        Stage::ScenePlan,
        Stage::Assets,
        Stage::Edit,
        Stage::Compose,
    ] {
        assert!(cp.is_complete(stage), "stage not complete: {stage:?}");
    }

    // 4. Final MP4 exists, is h264 1920x1080, duration matches composition.
    let mp4 = dir_path.join("render").join("final.mp4");
    assert!(mp4.exists(), "final.mp4 missing");
    let info = ffprobe_json(&mp4);
    let streams = info["streams"].as_array().unwrap();
    let v = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], 1920);
    assert_eq!(v["height"], 1080);
    // ponytail: brief seeded a narration line, so the rendered MP4 must
    // carry an audio stream (aac, voice track) — not a silent bed.
    let a = streams
        .iter()
        .find(|s| s["codec_type"] == "audio")
        .expect("audio stream missing — narration didn't make it into render");
    assert_eq!(a["codec_name"], "aac");
    let expected = comp.total_duration_s();
    let actual: f32 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (actual - expected).abs() < 0.5,
        "duration drift: expected {expected}, got {actual}"
    );
}

#[tokio::test]
async fn brief_parser_round_trip() {
    // ponytail: the parser is the only piece that knows brief format. Test it
    // here too so brief-format regressions don't hide behind pipeline tests.
    let b = kirkforge_video::pipelines::brief::parse_brief(
        "Title\nSubtitle\n- 50% claim\n- $4.2B market\n- 3.2x lift\n> brand.tag\n",
    );
    assert_eq!(b.title, "Title");
    assert_eq!(b.subtitle.as_deref(), Some("Subtitle"));
    assert_eq!(b.stats.len(), 3);
    assert_eq!(b.end_tag, "brand.tag");

    let plan = kirkforge_video::pipelines::brief::scene_plan_from_brief(&b);
    let kinds: Vec<&str> = plan["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[0], "hero_title");
    assert!(kinds.contains(&"stat_card"));
    assert!(kinds.contains(&"bar_chart"));
    assert_eq!(*kinds.last().unwrap(), "end_tag");
}

#[tokio::test]
async fn assets_stage_probes_clip_cut_via_analyzer() {
    // ponytail: the only piece that exercises the orchestrator → ToolRegistry
    // → Analyzer → ffprobe pipeline. Seed a real clip on disk, mark all
    // earlier stages complete in the checkpoint so ScenePlan is skipped,
    // then run from Assets.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // Seed a real clip via ffmpeg's testsrc filter.
    let clip = dir_path.join("seed.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=30",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success(), "seed clip generation failed");

    // Scene plan with one clip_cut pointing at the seeded clip.
    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Probe", "duration_s": 1.0},
                {"type": "clip_cut", "src": clip.to_string_lossy(), "in_s": 0.0, "out_s": 2.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    // Mark Research..ScenePlan complete so the orchestrator starts at Assets.
    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    let stamp = || "epoch:0".to_string();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: stamp(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline from Assets");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("asset_manifest.json")).unwrap())
            .unwrap();

    let clip_entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "clip_cut")
        .expect("clip_cut entry missing");
    assert_eq!(clip_entry["source"], "external");
    let probed = clip_entry["probed"]
        .as_object()
        .expect("probed metadata missing");
    let streams = probed["streams"].as_array().unwrap();
    let v = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], 320);
    assert_eq!(v["height"], 240);
}

#[tokio::test]
async fn edit_stage_flags_high_risk_for_trimming() {
    // ponytail: many text-only scenes push slideshow risk → Edit must emit a
    // cut. Confirms the Edit stage reads risk + suggests an action.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // 6 hero_title scenes → weak_shot_intent + typography_overreliance spike.
    let scenes: Vec<_> = (0..6)
        .map(|i| {
            serde_json::json!({
                "type": "hero_title", "title": format!("slide {i}"), "duration_s": 2.0
            })
        })
        .collect();
    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan", "scenes": scenes
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline from Assets");

    let dec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("edit_decisions.json")).unwrap())
            .unwrap();
    let avg = dec["risk_summary"]["average"].as_f64().unwrap();
    let cuts = dec["cuts"].as_array().unwrap();
    assert!(avg >= 3.0, "expected high risk, got {avg}");
    assert!(!cuts.is_empty(), "expected trim cut, got none");
    assert_eq!(dec["cuts"][0]["action"], "trim");
}

#[tokio::test]
async fn full_pipeline_auto_transcodes_non_h264_clip() {
    // ponytail: end-to-end proof that a mpeg4 source clip gets transcoded
    // and the final MP4 is still h264. Without auto-transcode the filter
    // graph would reject the source and Compose would fail.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};
    use std::process::Command;

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // Seed a mpeg4 (non-h264) clip.
    let clip = dir_path.join("seed.mpeg4.avi");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=30",
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
        ])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success(), "seed mpeg4 generation failed");

    // Scene plan references the mpeg4 clip.
    let plan_path = arts.join("scene_plan.json");
    std::fs::write(
        &plan_path,
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Auto", "duration_s": 1.0},
                {"type": "clip_cut", "src": clip.to_string_lossy(), "in_s": 0.0, "out_s": 1.5},
                {"type": "end_tag", "title": "End", "duration_s": 1.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    // Skip earlier stages so the ScenePlan we just wrote isn't replaced.
    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: plan_path.to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline from Assets");

    // Manifest should record an applied transcode.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir_path.join("artifacts/asset_manifest.json")).unwrap(),
    )
    .unwrap();
    let clip_entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "clip_cut")
        .expect("clip_cut entry missing");
    assert_eq!(clip_entry["transcode"]["applied"], true);

    // Final MP4 should be valid h264 1920x1080.
    let mp4 = dir_path.join("render/final.mp4");
    assert!(mp4.exists(), "final.mp4 missing");
    let ff = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(&mp4)
        .output()
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&ff.stdout).unwrap();
    let v = info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], 1920);
    assert_eq!(v["height"], 1080);
}

#[tokio::test]
async fn brand_kit_palette_overrides_bar_colors() {
    // ponytail: a project with brand.json gets its palette applied to the
    // bar chart colors. Without brand.json the default palette is used.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // Brand kit: single red accent. All default bar colors get replaced.
    std::fs::write(
        dir_path.join("brand.json"),
        serde_json::json!({
            "palette": ["#ff3366", "#ff3366", "#ff3366"]
        })
        .to_string(),
    )
    .unwrap();

    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Brand", "duration_s": 1.0},
                {"type": "bar_chart", "title": "x",
                 "bars": [
                    {"label": "a", "value": 0.5, "color": "#3aa0ff"},
                    {"label": "b", "value": 0.7, "color": "#3aa0ff"},
                    {"label": "c", "value": 0.9, "color": "#3aa0ff"}
                 ],
                 "duration_s": 2.0},
                {"type": "end_tag", "title": "End", "duration_s": 1.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline");

    let comp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("composition.json")).unwrap())
            .unwrap();
    let chart = &comp["scenes"][1];
    let bars = chart["bars"].as_array().unwrap();
    assert_eq!(bars.len(), 3);
    for (i, b) in bars.iter().enumerate() {
        assert_eq!(b["color"], "#ff3366", "bar {i} should be brand red");
    }
}

#[tokio::test]
async fn assets_stage_plans_transcode_for_non_h264_clips() {
    // ponytail: a mpeg4 / prores / webm clip would crash the ffmpeg filter
    // graph. Assets stage should detect the codec mismatch and record a
    // transcode plan so the user (or a follow-up stage) can normalize.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // Seed a non-h264 clip — mpeg4 with yuv420p. AVIs / webm / prores would
    // behave the same; mpeg4 is the most widely available alt codec.
    let clip = dir_path.join("seed.mpeg4.avi");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=160x120:rate=15",
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
        ])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success(), "seed mpeg4 generation failed");

    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Probe", "duration_s": 1.0},
                {"type": "clip_cut", "src": clip.to_string_lossy(), "in_s": 0.0, "out_s": 1.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline from Assets");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("asset_manifest.json")).unwrap())
            .unwrap();
    let clip_entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "clip_cut")
        .expect("clip_cut entry missing");
    let tc = clip_entry["transcode"]
        .as_object()
        .expect("transcode plan missing for non-h264 source");
    assert_eq!(tc["needed"], true);
    assert_eq!(tc["tool"], "transcoder");
    assert_eq!(tc["operation"], "transcode");
    let reason = tc["reason"].as_str().unwrap();
    assert!(
        reason.contains("mpeg4"),
        "reason should mention codec, got: {reason}"
    );
}

#[tokio::test]
async fn assets_stage_skips_transcode_for_h264() {
    // ponytail: counterpart to the test above — a clip that's already h264
    // yuv420p must NOT have a transcode plan.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    let clip = dir_path.join("seed.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=160x120:rate=15",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success(), "seed h264 generation failed");

    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Probe", "duration_s": 1.0},
                {"type": "clip_cut", "src": clip.to_string_lossy(), "in_s": 0.0, "out_s": 1.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline from Assets");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("asset_manifest.json")).unwrap())
            .unwrap();
    let clip_entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "clip_cut")
        .expect("clip_cut entry missing");
    assert!(
        clip_entry.get("transcode").is_none(),
        "transcode plan should be absent for h264 yuv420p, got: {}",
        clip_entry["transcode"]
    );
}

#[tokio::test]
async fn shot_language_round_trips_through_pipeline() {
    // ponytail: a scene_plan that authors shot language should preserve it
    // all the way to composition.json. Currently the renderer ignores the
    // field, but a future camera-motion pass needs the data.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "Shot", "duration_s": 1.0,
                 "shot": {"shot_type": "wide", "camera_motion": "push", "narrative_role": "setup"}},
                {"type": "end_tag", "title": "End", "duration_s": 1.0,
                 "shot": {"narrative_role": "bookend"}}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
        Stage::Assets,
        Stage::Edit,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline");

    let comp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(arts.join("composition.json")).unwrap())
            .unwrap();
    let hero = &comp["scenes"][0];
    assert_eq!(hero["shot"]["camera_motion"], "push");
    assert_eq!(hero["shot"]["shot_type"], "wide");
    assert_eq!(hero["shot"]["narrative_role"], "setup");
    let end = &comp["scenes"][1];
    assert_eq!(end["shot"]["narrative_role"], "bookend");
}

#[tokio::test]
async fn compose_refuses_unimplemented_render_runtime() {
    // ponytail: proposal_packet.json may request a different backend. The
    // current build renders with ffmpeg; Remotion / HyperFrames are reserved
    // and must fail loudly rather than silently fall back.
    use kirkforge_video::orchestrator::checkpoint::{Checkpoint, StageRecord};

    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    let arts = dir_path.join("artifacts");
    std::fs::create_dir_all(&arts).unwrap();

    // Proposal asks for Remotion. StagePlan + an AudioSpec for Compose.
    std::fs::write(
        arts.join("proposal_packet.json"),
        serde_json::json!({
            "kind": "proposal_packet",
            "render_runtime": "remotion",
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        arts.join("scene_plan.json"),
        serde_json::json!({
            "kind": "scene_plan",
            "scenes": [
                {"type": "hero_title", "title": "x", "duration_s": 1.0},
                {"type": "end_tag",   "title": "y", "duration_s": 1.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut cp = Checkpoint::load_or_init(&dir_path, "animated_explainer").unwrap();
    for stage in [
        Stage::Research,
        Stage::Proposal,
        Stage::Script,
        Stage::ScenePlan,
        Stage::Assets,
        Stage::Edit,
    ] {
        cp.records.insert(
            stage,
            StageRecord {
                artifact: arts.join("scene_plan.json").to_string_lossy().into_owned(),
                completed_at: "epoch:0".into(),
            },
        );
    }
    cp.save(&dir_path).unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    let err = kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect_err("remotion runtime must refuse");
    let msg = format!("{err}");
    assert!(
        msg.contains("remotion") || msg.contains("Remotion"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn transitions_render_with_xfade_filter() {
    // ponytail: when a scene declares shot.transition, the rendered MP4
    // must come from the xfade chain (no concat). End-to-end render check.
    use kirkforge_video::compose::{
        build_filter_graph, render_composition, AudioSpec, Composition, Scene, ShotMeta,
        TransitionSpec,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("xfade.mp4");
    let comp = Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes: vec![
            Scene::HeroTitle {
                text: "A".into(),
                subtitle: None,
                duration_s: 2.0,
                shot: Some(ShotMeta {
                    shot_type: None,
                    camera_motion: None,
                    narrative_role: None,
                    transition: Some(TransitionSpec {
                        kind: "fade".into(),
                        duration_s: 0.5,
                    }),
                }),
            },
            Scene::HeroTitle {
                text: "B".into(),
                subtitle: None,
                duration_s: 2.0,
                shot: None,
            },
            Scene::HeroTitle {
                text: "C".into(),
                subtitle: None,
                duration_s: 2.0,
                shot: None,
            },
        ],
        audio: Some(AudioSpec::Silent),
    };
    let plan = build_filter_graph(&comp.scenes, comp.width, comp.height, comp.fps);
    assert!(
        plan.filter_complex.contains("xfade=transition=fade"),
        "expected xfade chain:\n{}",
        plan.filter_complex
    );
    render_composition(&comp, &out)
        .await
        .expect("render with transitions");
    assert!(out.exists(), "xfade.mp4 should exist");
    let info = ffprobe_json(&out);
    let v = info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap();
    assert_eq!(v["codec_name"], "h264");
    // xfade overlaps scenes by `transition.duration_s`, so total stays at
    // sum(durations) = 6.0 — both halves of the overlap cancel in atrim.
    let actual: f32 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (actual - 6.0).abs() < 0.3,
        "xfade render duration drift: expected ~6.0, got {actual}"
    );
}

#[tokio::test]
async fn ken_burns_motion_renders_with_scale_crop_in_filter() {
    // ponytail: when scene_plan declares camera_motion: push, the rendered
    // filter_complex must include scale+crop to produce the slow zoom.
    // Test bypasses the pipeline by synthesizing the composition directly
    // so we can inject a specific motion and assert without spinning up
    // every stage.
    use kirkforge_video::compose::{
        build_filter_graph, render_composition, AudioSpec, Composition, Scene, ShotMeta,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("kenburns.mp4");
    let comp = Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes: vec![Scene::HeroTitle {
            text: "Push".into(),
            subtitle: None,
            duration_s: 2.0,
            shot: Some(ShotMeta {
                shot_type: None,
                camera_motion: Some("push".into()),
                narrative_role: None,
                transition: None,
            }),
        }],
        audio: Some(AudioSpec::Silent),
    };
    let plan = build_filter_graph(&comp.scenes, comp.width, comp.height, comp.fps);
    assert!(
        plan.filter_complex.contains("scale=2208:1242"),
        "expected push scale in filter complex:\n{}",
        plan.filter_complex
    );
    render_composition(&comp, &out)
        .await
        .expect("render with motion");
    let info = ffprobe_json(&out);
    let v = info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], 1920);
    assert_eq!(v["height"], 1080);
}

#[tokio::test]
async fn caption_overlay_writes_srt_sidecar_and_embeds_subtitle_stream() {
    // ponytail: when a Composition has CaptionOverlay scenes, render
    // must write `<out>.srt` AND mux a mov_text subtitle stream into
    // the MP4. Players like VLC can toggle that stream on/off.
    use kirkforge_video::compose::{render_composition, AudioSpec, Composition, Scene};
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("captions.mp4");
    let comp = Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes: vec![
            Scene::CaptionOverlay {
                lines: vec!["Hello".into(), "world".into()],
                duration_s: 2.0,
                shot: None,
            },
            Scene::EndTag {
                text: "bye".into(),
                duration_s: 1.0,
                shot: None,
            },
        ],
        audio: Some(AudioSpec::Silent),
    };
    render_composition(&comp, &out)
        .await
        .expect("render captions");

    // 1. sidecar file exists.
    let srt = dir.path().join("captions.srt");
    assert!(srt.exists(), "captions.srt sidecar should exist");
    let srt_body = std::fs::read_to_string(&srt).expect("read srt");
    assert!(
        srt_body.contains("Hello"),
        "srt must contain Hello: {srt_body}"
    );
    assert!(srt_body.contains("world"), "srt must contain world");
    assert!(
        srt_body.contains("00:00:00,000 --> 00:00:01,000"),
        "first caption slice must be 1s: {srt_body}"
    );

    // 2. subtitle stream muxed into the MP4.
    let info = ffprobe_json(&out);
    let subs: Vec<_> = info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["codec_type"] == "subtitle")
        .collect();
    assert_eq!(
        subs.len(),
        1,
        "expected 1 subtitle stream, got {}",
        subs.len()
    );
    assert_eq!(subs[0]["codec_name"], "mov_text");
}

#[tokio::test]
async fn media_profile_tiktok_renders_vertical_1080x1920() {
    // ponytail: when a Composition is rendered with the tiktok profile
    // applied (1080×1920 portrait, 30 fps, h264 yuv420p), the MP4 must
    // match exactly — width / height / fps come from the profile, not the
    // composition's authored resolution.
    use kirkforge_video::compose::{
        apply_to_composition, get_profile, render_composition, AudioSpec, Composition, Scene,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("tiktok.mp4");
    let mut comp = Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes: vec![Scene::HeroTitle {
            text: "V".into(),
            subtitle: None,
            duration_s: 1.0,
            shot: None,
        }],
        audio: Some(AudioSpec::Silent),
    };
    let profile = get_profile("tiktok").expect("tiktok profile");
    apply_to_composition(profile, &mut comp);
    assert_eq!(comp.width, 1080);
    assert_eq!(comp.height, 1920);
    render_composition(&comp, &out)
        .await
        .expect("render tiktok");
    let info = ffprobe_json(&out);
    let v = info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(v["width"], 1080);
    assert_eq!(v["height"], 1920);
    assert_eq!(v["pix_fmt"], "yuv420p");
}

#[tokio::test]
async fn media_profile_unknown_name_errors() {
    // ponytail: get_profile is fallible (returns Option). Callers must
    // surface a clear error naming the available profiles.
    use kirkforge_video::compose::{get_profile, ALL_PROFILES};
    let unknown = get_profile("not_a_real_platform_xyz");
    assert!(unknown.is_none());
    // Confirm the documented set exists.
    let names: Vec<&str> = ALL_PROFILES.iter().map(|p| p.name).collect();
    assert!(names.contains(&"tiktok"));
    assert!(names.contains(&"youtube_shorts"));
    assert!(names.contains(&"cinematic"));
}

#[tokio::test]
async fn narration_stage_skipped_when_brief_has_no_narration() {
    // ponytail: a brief with only the title (no second-line body) means
    // script.json gets an empty narration field. The Narration stage must
    // skip synthesis and Compose must keep the silent audio bed.
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    std::fs::write(dir_path.join("brief.txt"), "Title Only\n").unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline");

    let arts = dir_path.join("artifacts");
    assert!(
        !arts.join("narration.mp3").exists(),
        "narration.mp3 should not exist when brief has no narration body"
    );
    let comp: Composition =
        serde_json::from_str(&std::fs::read_to_string(arts.join("composition.json")).unwrap())
            .expect("composition parses");
    matches!(
        comp.audio,
        Some(kirkforge_video::compose::AudioSpec::Silent)
    );
}

#[tokio::test]
async fn narration_stage_synthesizes_voice_from_brief_body() {
    // ponytail: when the brief has a second line, the Narration stage
    // synthesizes narration.mp3 via ffmpeg libflite (offline TTS). The
    // decision log must contain a `narration` category entry explaining
    // the backend choice.
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    std::fs::write(
        dir_path.join("brief.txt"),
        "Hello World\nThis is the voiceover line.\n",
    )
    .unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect("pipeline");

    let arts = dir_path.join("artifacts");
    assert!(arts.join("narration.mp3").exists(), "narration.mp3 missing");
    let log = std::fs::read_to_string(arts.join("decision_log.jsonl")).unwrap();
    assert!(
        log.contains("\"category\":\"narration\""),
        "decision log missing narration entry:\n{log}"
    );
    assert!(
        log.contains("\"choice\":\"flite\""),
        "decision log should record flite backend:\n{log}"
    );
}

#[tokio::test]
async fn motion_led_promise_requires_provider_key() {
    // ponytail: when the brief promises motion-led delivery but no provider
    // key is set, the Proposal stage MUST refuse — silently downgrading to
    // still-led is the failure mode this PromiseType guards against.
    if std::env::var("VEO_API_KEY").is_ok() || std::env::var("RUNWAY_API_KEY").is_ok() {
        eprintln!("skipping — provider key set in this shell");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_path_buf();
    std::fs::write(dir_path.join("promise.json"), "\"motion_led\"").unwrap();

    let pipe = AnimatedExplainer;
    let reg = ToolRegistry::with_builtins();
    let err = kirkforge_video::orchestrator::run_pipeline(&pipe, &dir_path, &reg)
        .await
        .expect_err("motion_led without provider key must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("motion_led") || msg.contains("MotionLed"),
        "unexpected error: {msg}"
    );
}
