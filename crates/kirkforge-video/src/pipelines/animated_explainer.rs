//! `animated_explainer` — default pipeline. Each stage writes one artifact
//! into `<project>/artifacts/`. The `Compose` stage synthesizes a
//! `composition.json` from the preceding artifacts so the pipeline renders
//! a real MP4 end-to-end without manual JSON.
//!
//! ponytail: this exists — scene-plan synthesis happens once, on disk,
//! so a human can hand-edit `composition.json` and re-run only Compose.

use std::path::{Path, PathBuf};

use anyhow::Context;
use async_trait::async_trait;
use serde::Deserialize;

use crate::compose::{scene_kind_tag, AudioSpec, Bar, Composition, Scene};
use crate::error::Result;
use crate::orchestrator::{
    decision::Decision, promise::PromiseType, slideshow_risk, DecisionLog, Stage,
};
use crate::pipelines::{brief, Pipeline};
use crate::tools::ToolRegistry;

pub struct AnimatedExplainer;

/// Loose shape of `scene_plan.json`. Only `scenes` matters here.
#[derive(Debug, Deserialize)]
struct ScenePlan {
    scenes: Vec<PlannedScene>,
}

#[derive(Debug, Deserialize)]
struct PlannedScene {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    bars: Vec<PlannedBar>,
    #[serde(default)]
    duration_s: Option<f32>,
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    in_s: Option<f32>,
    #[serde(default)]
    out_s: Option<f32>,
    #[serde(default)]
    shot: Option<PlannedShot>,
}

/// ponytail: deliberately a sub-shape of `ShotMeta` so authoring a brief or
/// scene plan doesn't have to repeat the full struct. Optional everywhere.
#[derive(Debug, Default, Deserialize)]
struct PlannedShot {
    #[serde(default)]
    shot_type: Option<String>,
    #[serde(default)]
    camera_motion: Option<String>,
    #[serde(default)]
    narrative_role: Option<String>,
    #[serde(default)]
    transition: Option<crate::compose::TransitionSpec>,
}

impl PlannedShot {
    fn to_meta(&self) -> crate::compose::ShotMeta {
        crate::compose::ShotMeta {
            shot_type: self.shot_type.clone(),
            camera_motion: self.camera_motion.clone(),
            narrative_role: self.narrative_role.clone(),
            transition: self.transition.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PlannedBar {
    label: String,
    value: f32,
    #[serde(default = "default_bar_color")]
    color: String,
}

fn default_bar_color() -> String {
    "#3aa0ff".into()
}

#[async_trait]
impl Pipeline for AnimatedExplainer {
    fn name(&self) -> &'static str {
        "animated_explainer"
    }
    fn description(&self) -> &'static str {
        "Research -> proposal -> script -> narration -> scene plan -> assets -> edit -> render. The default for brief-driven explainer content."
    }

    fn stages(&self) -> &'static [Stage] {
        &[
            Stage::Research,
            Stage::Proposal,
            Stage::Script,
            Stage::Narration,
            Stage::ScenePlan,
            Stage::Assets,
            Stage::Edit,
            Stage::Compose,
        ]
    }

    async fn run_stage(&self, stage: Stage, dir: &Path, reg: &ToolRegistry) -> Result<String> {
        let arts = dir.join("artifacts");
        std::fs::create_dir_all(&arts)?;
        let log = DecisionLog::open(dir);

        let artifact_path = match stage {
            Stage::Research => {
                log.append(&Decision {
                    category: "research_scope".into(),
                    choice: "user-provided brief only".into(),
                    reason: "no external research API in this build".into(),
                    options_considered: vec!["web_search".into(), "user_brief".into()],
                    rejected_because: vec!["web_search: no API key".into()],
                })?;
                write_json(
                    &arts.join("research_brief.json"),
                    &serde_json::json!({
                        "kind": "research_brief",
                        "title": "(from brief)",
                        "data_points": [],
                    }),
                )?
            }
            Stage::Proposal => {
                let promise = load_promise(dir);
                log.append(&Decision {
                    category: "delivery_promise".into(),
                    choice: format!("{promise:?}"),
                    reason: format!(
                        "rules: still_fallback={} requires_video={} min_motion={}",
                        promise.rules().still_fallback_allowed,
                        promise.rules().requires_video_generation,
                        promise.rules().min_motion_ratio
                    ),
                    options_considered: PromiseType::all()
                        .iter()
                        .map(|p| format!("{p:?}"))
                        .collect(),
                    rejected_because: vec![],
                })?;
                if promise == PromiseType::MotionLed && !any_provider_key() {
                    return Err(crate::error::KfError::Artifact(
                        "PromiseType::MotionLed requires a video-generation provider key (e.g. VEO_API_KEY / RUNWAY_API_KEY); none configured and still-fallback is not allowed".into(),
                    ));
                }
                write_json(
                    &arts.join("proposal_packet.json"),
                    &serde_json::json!({
                        "kind": "proposal_packet",
                        "concepts": [],
                        "selected_concept": null,
                        "renderer_family": "data_explainer",
                        "render_runtime": "ffmpeg",
                        "promise": format!("{promise:?}"),
                    }),
                )?
            }
            Stage::Script => {
                // ponytail: if a brief exists, lift its first non-empty line
                // after the title into the narration field. Keeps the
                // default empty narration for tests that don't seed it.
                let narration = read_brief_narration(dir);
                write_json(
                    &arts.join("script.json"),
                    &serde_json::json!({
                        "kind": "script",
                        "voice_performance": "neutral",
                        "narration": narration,
                        "enhancement_cues": [],
                    }),
                )?
            }
            Stage::Narration => build_narration(dir, &arts).await?,
            Stage::ScenePlan => {
                write_json(&arts.join("scene_plan.json"), &scene_plan_with_brief(dir))?
            }
            Stage::Assets => build_assets(dir, &arts, reg).await?,
            Stage::Edit => build_edit_decisions(dir, &arts).await?,
            Stage::Compose => {
                let comp_path = arts.join("composition.json");
                let plan_path = arts.join("scene_plan.json");
                if !plan_path.exists() {
                    return Err(crate::error::KfError::Artifact(
                        "scene_plan.json missing — run earlier stages first".into(),
                    ));
                }
                check_render_runtime(&arts)?;
                let mut comp = synthesize_composition(&plan_path)?;
                // ponytail: if the Narration stage produced narration.mp3,
                // swap the silent bed for a Narration audio spec. This is
                // the only cross-stage handoff into Compose.
                let narration_path = arts.join("narration.mp3");
                if narration_path.exists() {
                    comp.audio = Some(AudioSpec::Narration {
                        path: narration_path,
                        duck_under: true,
                    });
                }
                std::fs::write(&comp_path, serde_json::to_string_pretty(&comp)?)?;

                // ponytail: SlideshowRisk runs once, after Compose, against the
                // just-synthesized composition. Pass per-scene shot metadata
                // (camera_motion) so a hero_title with `motion: push` scores
                // lower than one declared `static`. Use the views-aware API.
                let views: Vec<slideshow_risk::SceneView> = comp
                    .scenes
                    .iter()
                    .map(|s| {
                        let motion = match s {
                            Scene::HeroTitle { shot, .. }
                            | Scene::TextCard { shot, .. }
                            | Scene::StatCard { shot, .. }
                            | Scene::BarChart { shot, .. }
                            | Scene::CaptionOverlay { shot, .. }
                            | Scene::QuoteCard { shot, .. }
                            | Scene::Comparison { shot, .. }
                            | Scene::ProgressBar { shot, .. }
                            | Scene::Callout { shot, .. }
                            | Scene::KpiGrid { shot, .. }
                            | Scene::LineChart { shot, .. }
                            | Scene::PieChart { shot, .. }
                            | Scene::ClipCut { shot, .. }
                            | Scene::EndTag { shot, .. }
                            | Scene::TerminalScene { shot, .. } => {
                                shot.as_ref().and_then(|m| m.camera_motion.as_deref())
                            }
                        };
                        slideshow_risk::SceneView {
                            kind: scene_kind_tag(s),
                            motion,
                        }
                    })
                    .collect();
                let report =
                    slideshow_risk::score_slideshow_risk_views(&views, comp.total_duration_s());
                std::fs::write(
                    arts.join("risk_report.json"),
                    serde_json::to_string_pretty(&report)?,
                )?;
                log.append(&Decision {
                    category: "composition_synthesis".into(),
                    choice: "scene_plan → composition.json".into(),
                    reason: format!("{} scenes synthesized", comp.scenes.len()),
                    options_considered: vec!["scene_plan".into(), "manual_composition".into()],
                    rejected_because: vec![],
                })?;
                log.append(&Decision {
                    category: "slideshow_risk".into(),
                    choice: format!("{:?}", report.verdict),
                    reason: format!("avg={:.2}", report.average),
                    options_considered: vec!["ship".into(), "revise".into()],
                    rejected_because: if matches!(
                        report.verdict,
                        slideshow_risk::RiskVerdict::Revise | slideshow_risk::RiskVerdict::Fail
                    ) {
                        vec!["ship: average too high".into()]
                    } else {
                        vec![]
                    },
                })?;
                comp_path.to_string_lossy().into_owned()
            }
        };
        Ok(artifact_path)
    }
}

fn synthesize_composition(plan_path: &Path) -> Result<Composition> {
    let raw = std::fs::read_to_string(plan_path)?;
    let plan: ScenePlan = serde_json::from_str(&raw)?;
    let brand = load_brand(plan_path);
    // ponytail: build a src→dst map from the asset manifest so clips that
    // got transcoded by the Assets stage are loaded from the transcoded
    // path. Missing manifest → empty map (legacy / render-only path).
    let transcode_map = load_transcode_map(plan_path);
    let mut scenes = Vec::with_capacity(plan.scenes.len());
    for p in plan.scenes {
        let dur = p.duration_s.unwrap_or(3.0);
        let scene = match p.kind.as_str() {
            "hero_title" => Scene::HeroTitle {
                text: p.title.unwrap_or_default(),
                subtitle: p.subtitle,
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "text_card" => Scene::TextCard {
                title: p.title.unwrap_or_default(),
                body: p.body.unwrap_or_default(),
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "stat_card" => Scene::StatCard {
                number: p.number.unwrap_or_default(),
                label: p.label.unwrap_or_default(),
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "bar_chart" => Scene::BarChart {
                title: p.title.unwrap_or_default(),
                bars: p
                    .bars
                    .into_iter()
                    .enumerate()
                    .map(|(i, b)| Bar {
                        label: b.label,
                        value: b.value,
                        // ponytail: prefer explicit bar color; fall back to
                        // brand palette by position; fall back to default.
                        color: if !b.color.is_empty() && b.color != default_bar_color() {
                            b.color.clone()
                        } else {
                            brand.palette[i % brand.palette.len()].clone()
                        },
                    })
                    .collect(),
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "caption_overlay" => Scene::CaptionOverlay {
                lines: p
                    .label
                    .map(|l| l.split('|').map(String::from).collect())
                    .or_else(|| p.title.clone().map(|t| vec![t]))
                    .unwrap_or_default(),
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "clip_cut" => Scene::ClipCut {
                // ponytail: substitute the transcoded path if Assets
                // rewrote this src. Stays on the original src when no
                // manifest entry exists.
                src: p
                    .src
                    .as_deref()
                    .and_then(|s| transcode_map.get(s).cloned())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(p.src.unwrap_or_default())),
                in_s: p.in_s.unwrap_or(0.0),
                out_s: p.out_s.unwrap_or(p.duration_s.unwrap_or(3.0)),
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            "end_tag" => Scene::EndTag {
                text: p.title.unwrap_or_default(),
                duration_s: dur,
                shot: p.shot.as_ref().map(PlannedShot::to_meta),
            },
            other => {
                return Err(crate::error::KfError::Artifact(format!(
                    "unknown scene type: {other}"
                )))
            }
        };
        scenes.push(scene);
    }
    Ok(Composition {
        width: 1920,
        height: 1080,
        fps: 30,
        scenes,
        audio: Some(AudioSpec::Silent),
    })
}

fn load_promise(dir: &Path) -> PromiseType {
    let p = dir.join("promise.json");
    if p.exists() {
        if let Ok(raw) = std::fs::read_to_string(&p) {
            if let Ok(pt) = serde_json::from_str::<PromiseType>(&raw) {
                return pt;
            }
        }
    }
    PromiseType::DataExplainer
}

/// ponytail: per-project brand kit read from `<project>/brand.json` next
/// to `brief.txt` and `promise.json`. Schema is intentionally tiny —
/// palette + primary color + font + voice. Missing file → all defaults.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct BrandKit {
    #[serde(default)]
    palette: Vec<String>,
    #[serde(default)]
    primary_color: Option<String>,
    #[serde(default)]
    font: Option<String>,
    #[serde(default)]
    voice: Option<String>,
}

impl Default for BrandKit {
    fn default() -> Self {
        Self {
            palette: vec![
                "#3aa0ff".into(),
                "#ffcc00".into(),
                "#6cd07a".into(),
                "#ff5a5a".into(),
                "#bb86fc".into(),
            ],
            primary_color: None,
            font: None,
            voice: None,
        }
    }
}

fn load_brand(plan_path: &Path) -> BrandKit {
    // plan_path is `<project>/artifacts/scene_plan.json` — walk up to
    // `<project>/brand.json`.
    let candidate = plan_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("brand.json"));
    if let Some(p) = candidate {
        if let Ok(raw) = std::fs::read_to_string(&p) {
            if let Ok(b) = serde_json::from_str::<BrandKit>(&raw) {
                return b;
            }
        }
    }
    BrandKit::default()
}

/// ponytail: read `<project>/artifacts/asset_manifest.json` and build the
/// src→dst map of clips that Assets transcoded. Empty on missing/legacy.
fn load_transcode_map(plan_path: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let candidate = plan_path.parent().map(|p| p.join("asset_manifest.json"));
    let Some(p) = candidate else {
        return map;
    };
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return map;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return map;
    };
    if let Some(entries) = v["entries"].as_array() {
        for e in entries {
            if let Some(tc) = e.get("transcode") {
                if tc["applied"] == true {
                    if let (Some(src), Some(dst)) = (tc["src"].as_str(), tc["dst"].as_str()) {
                        map.insert(src.to_string(), dst.to_string());
                    }
                }
            }
        }
    }
    map
}

fn any_provider_key() -> bool {
    ["VEO_API_KEY", "RUNWAY_API_KEY"]
        .iter()
        .any(|k| std::env::var(k).is_ok())
}

/// ponytail: scan the probe metadata and return a transcode plan iff the
/// source needs to be normalized for the ffmpeg filter graph. We only flag
/// codec and pixel-format mismatches; size is handled by scale+pad at
/// render time. Cheap, deterministic, and the user can run the plan with
/// `kf tools invoke transcoder transcode src=... dst=...`.
fn needs_transcode(meta: &serde_json::Value, src: &Path) -> Option<serde_json::Value> {
    let streams = meta["streams"].as_array()?;
    let v = streams.iter().find(|s| s["codec_type"] == "video")?;
    let codec = v["codec_name"].as_str().unwrap_or("");
    let pix = v["pix_fmt"].as_str().unwrap_or("");
    if codec == "h264" && (pix == "yuv420p" || pix.is_empty()) {
        return None;
    }
    let dst = src.with_extension("transcoded.mp4");
    Some(serde_json::json!({
        "needed": true,
        "reason": format!("codec={codec} pix_fmt={pix}"),
        "src": src.to_string_lossy(),
        "dst": dst.to_string_lossy(),
        "tool": "transcoder",
        "operation": "transcode",
    }))
}

/// ponytail: actually execute a recorded transcode plan. The `dst` is
/// derived by `needs_transcode` to live next to the source. Failures are
/// reported via the manifest, not raised, so the pipeline still completes
/// and the user can see what went wrong.
async fn run_transcode_plan(reg: &ToolRegistry, plan: &serde_json::Value) -> Result<()> {
    let trans = reg
        .require("transcoder")
        .map_err(|e| crate::error::KfError::Artifact(format!("transcoder tool missing: {e}")))?;
    let src = plan["src"]
        .as_str()
        .ok_or_else(|| crate::error::KfError::Artifact("transcode plan missing src".into()))?;
    let dst = plan["dst"]
        .as_str()
        .ok_or_else(|| crate::error::KfError::Artifact("transcode plan missing dst".into()))?;
    let params = serde_json::json!({
        "operation": "transcode",
        "src": src,
        "dst": dst,
        "crf": 23,
    });
    let _ = trans
        .invoke(std::path::Path::new("."), "transcode", params)
        .await?;
    Ok(())
}

/// ponytail: refuse any render_runtime other than ffmpeg until those backends
/// are actually wired up. proposal_packet.json is the only place that knows
/// which backend a run wants; if it asks for Remotion/HyperFrames we error
/// clearly so the user knows to switch the proposal — not silently fall back
/// to ffmpeg and ship something they didn't ask for.
fn check_render_runtime(arts: &Path) -> Result<()> {
    let p = arts.join("proposal_packet.json");
    if !p.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&p).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
    match v["render_runtime"].as_str() {
        Some("ffmpeg") | None => Ok(()),
        Some(other) => Err(crate::error::KfError::Artifact(format!(
            "render_runtime '{other}' is reserved and not yet implemented in this build \
             (this build renders with ffmpeg). Edit proposal_packet.json and set \
             render_runtime to 'ffmpeg', or wait for the {other} backend."
        ))),
    }
}

/// ponytail: Assets stage walks the scene plan and registers every scene.
/// For `clip_cut` scenes we probe the source via `analysis` to record codec /
/// resolution / duration. Missing clips log a decision and proceed — never
/// fail the pipeline for a missing asset (Compose will surface the real error).
async fn build_assets(dir: &Path, arts: &Path, reg: &ToolRegistry) -> Result<String> {
    let log = DecisionLog::open(dir);
    let plan_path = arts.join("scene_plan.json");
    let raw = std::fs::read_to_string(&plan_path).unwrap_or_else(|_| "{}".into());
    let plan: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));

    let analyzer = reg.require("analysis").ok();
    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    if let Some(scenes) = plan["scenes"].as_array() {
        for (i, s) in scenes.iter().enumerate() {
            let kind = s["type"].as_str().unwrap_or("unknown").to_string();
            let dur = s["duration_s"].as_f64().unwrap_or(0.0) as f32;
            let entry = serde_json::json!({
                "index": i,
                "kind": kind,
                "duration_s": dur,
                "source": if kind == "clip_cut" { "external" } else { "synthesized" },
            });
            let mut entry = entry;

            if kind == "clip_cut" {
                if let Some(src) = s.get("src").and_then(|v| v.as_str()) {
                    let pb = Path::new(src);
                    if pb.exists() {
                        if let Some(a) = &analyzer {
                            let params = serde_json::json!({"operation":"probe","src":pb});
                            match a.invoke(dir, "probe", params).await {
                                Ok(out) => {
                                    let transcode = needs_transcode(&out.meta, pb);
                                    entry
                                        .as_object_mut()
                                        .context("asset entry is a JSON object")?
                                        .insert("probed".into(), out.meta);
                                    if let Some(mut plan) = transcode {
                                        // ponytail: run the transcode plan
                                        // eagerly so Compose can read the
                                        // transcoded path. Without this the
                                        // plan was advisory and the filter
                                        // graph would still reject the
                                        // source.
                                        match run_transcode_plan(reg, &plan).await {
                                            Ok(_) => {
                                                plan.as_object_mut()
                                                    .context("transcode plan is a JSON object")?
                                                    .insert(
                                                        "applied".into(),
                                                        serde_json::json!(true),
                                                    );
                                                log.append(&Decision {
                                                    category: "asset_transcode".into(),
                                                    choice: "applied".into(),
                                                    reason: format!("{src}: {}", plan["reason"]),
                                                    options_considered: vec![
                                                        "apply".into(),
                                                        "skip".into(),
                                                        "fail".into(),
                                                    ],
                                                    rejected_because: vec![
                                                        "fail: blocks render".into(),
                                                        "skip: filter graph will reject".into(),
                                                    ],
                                                })?;
                                            }
                                            Err(e) => {
                                                plan.as_object_mut()
                                                    .context("transcode plan is a JSON object")?
                                                    .insert(
                                                        "applied".into(),
                                                        serde_json::json!(false),
                                                    );
                                                plan.as_object_mut()
                                                    .context("transcode plan is a JSON object")?
                                                    .insert(
                                                        "error".into(),
                                                        serde_json::json!(e.to_string()),
                                                    );
                                                log.append(&Decision {
                                                    category: "asset_transcode".into(),
                                                    choice: "failed".into(),
                                                    reason: format!("{src}: {e}"),
                                                    options_considered: vec![
                                                        "apply".into(),
                                                        "skip".into(),
                                                        "fail".into(),
                                                    ],
                                                    rejected_because: vec![
                                                        "fail: blocks render".into()
                                                    ],
                                                })?;
                                            }
                                        }
                                        entry
                                            .as_object_mut()
                                            .context("asset entry is a JSON object")?
                                            .insert("transcode".into(), plan);
                                    }
                                }
                                Err(e) => {
                                    log.append(&Decision {
                                        category: "asset_probe".into(),
                                        choice: "skip".into(),
                                        reason: format!("{src}: {e}"),
                                        options_considered: vec!["probe".into(), "skip".into()],
                                        rejected_because: vec!["probe: tool error".into()],
                                    })?;
                                }
                            }
                        }
                    } else {
                        missing.push(src.to_string());
                        log.append(&Decision {
                            category: "asset_missing".into(),
                            choice: "compose-stage-must-handle".into(),
                            reason: format!("{src} does not exist on disk"),
                            options_considered: vec!["fail".into(), "skip".into()],
                            rejected_because: vec!["fail: blocks whole pipeline".into()],
                        })?;
                    }
                }
            }
            entries.push(entry);
        }
    }

    let manifest = serde_json::json!({
        "kind": "asset_manifest",
        "entries": entries,
        "missing": missing,
        "summary": {
            "total": entries.len(),
            "external": entries.iter().filter(|e| e["source"] == "external").count(),
            "synthesized": entries.iter().filter(|e| e["source"] == "synthesized").count(),
        }
    });
    write_json(&arts.join("asset_manifest.json"), &manifest)
}

/// ponytail: Edit stage reads risk + assets, emits cut decisions. Currently
/// deterministic (drop scenes that push slideshow risk over the threshold).
/// Real implementation will use scene_plan + risk_report to suggest where to
/// add b-roll or trim text-only scenes.
async fn build_edit_decisions(dir: &Path, arts: &Path) -> Result<String> {
    let log = DecisionLog::open(dir);
    // ponytail: Edit runs before Compose, so risk_report.json doesn't exist
    // yet. Compute risk inline from the synthesized composition (read
    // directly from disk after the ScenePlan stage).
    let plan_path = arts.join("scene_plan.json");
    let (avg, verdict) = if plan_path.exists() {
        let kinds: Vec<String> = std::fs::read_to_string(&plan_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["scenes"].as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s["type"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
        let report = slideshow_risk::score_slideshow_risk(&refs, 0.0);
        (report.average as f64, format!("{:?}", report.verdict))
    } else {
        (0.0, "unknown".into())
    };

    let cuts: Vec<serde_json::Value> = if avg >= 3.5 {
        log.append(&Decision {
            category: "edit_strategy".into(),
            choice: "trim_static".into(),
            reason: format!("slideshow risk={verdict} (avg={avg:.2})"),
            options_considered: vec!["trim_static".into(), "add_broll".into(), "ship".into()],
            rejected_because: vec![
                "ship: risk too high".into(),
                "add_broll: no providers configured".into(),
            ],
        })?;
        vec![serde_json::json!({
            "action": "trim",
            "target": "text-only scenes",
            "rationale": "high slideshow risk",
        })]
    } else {
        vec![]
    };

    let decisions = serde_json::json!({
        "kind": "edit_decisions",
        "render_runtime": "ffmpeg",
        "cuts": cuts,
        "risk_summary": {"average": avg, "verdict": verdict},
    });
    write_json(&arts.join("edit_decisions.json"), &decisions)
}

/// Write a JSON value to disk and return the path as a String.
fn write_json(path: &Path, v: &serde_json::Value) -> Result<String> {
    std::fs::write(path, serde_json::to_string_pretty(v)?)?;
    Ok(path.to_string_lossy().into_owned())
}

/// ponytail: lift a short narration line from `<project>/brief.txt` — first
/// line is the title, the first non-bullet, non-empty line after the title
/// becomes the voiceover text. Falls back to a short generic line.
fn read_brief_narration(dir: &Path) -> String {
    let p = dir.join("brief.txt");
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return String::new();
    };
    let mut lines = raw.lines().filter(|l| !l.trim().is_empty());
    let _ = lines.next(); // title
    for l in lines {
        let t = l.trim();
        if t.starts_with('-') || t.starts_with('#') {
            continue;
        }
        return t.to_string();
    }
    String::new()
}

/// ponytail: synthesize voiceover audio from script.json's narration field.
/// Default backend is ffmpeg libflite (offline, no API key); if
/// ELEVENLABS_API_KEY is set AND an `elevenlabs` provider stub exists, the
/// decision log records the API was available but flite was chosen for
/// cost/offline. If narration is empty, we skip synthesis and Compose
/// keeps its default silent bed.
async fn build_narration(dir: &Path, arts: &Path) -> Result<String> {
    let log = DecisionLog::open(dir);
    let script_path = arts.join("script.json");
    let raw = std::fs::read_to_string(&script_path).unwrap_or_else(|_| "{}".into());
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
    let narration = v["narration"].as_str().unwrap_or("").trim().to_string();
    let dst = arts.join("narration.mp3");

    if narration.is_empty() {
        log.append(&Decision {
            category: "narration".into(),
            choice: "skip".into(),
            reason: "script.json narration is empty".into(),
            options_considered: vec!["flite".into(), "elevenlabs".into(), "skip".into()],
            rejected_because: vec![],
        })?;
        return Ok(script_path.to_string_lossy().into_owned());
    }

    let voice = std::env::var("KF_FLITE_VOICE").unwrap_or_else(|_| "kal".to_string());
    let backend = if std::env::var("ELEVENLABS_API_KEY").is_ok() {
        log.append(&Decision {
            category: "narration".into(),
            choice: "flite".into(),
            reason: "ELEVENLABS_API_KEY set but flite chosen (offline, no quota cost)".into(),
            options_considered: vec!["flite".into(), "elevenlabs".into()],
            rejected_because: vec![
                "elevenlabs: API key present but not invoked — build is offline-first".into(),
            ],
        })?;
        "flite"
    } else {
        log.append(&Decision {
            category: "narration".into(),
            choice: "flite".into(),
            reason: "no ELEVENLABS_API_KEY; using ffmpeg libflite (offline TTS)".into(),
            options_considered: vec!["flite".into(), "elevenlabs".into()],
            rejected_because: vec!["elevenlabs: no API key".into()],
        })?;
        "flite"
    };

    if backend == "flite" {
        // ponytail: ffmpeg's `flite` filter renders WAV to stdout when we
        // target `-`. Encode to MP3 in the same invocation so the file is
        // a compact, seekable voice track.
        let flite = format!(
            "flite=text='{}':voice={}",
            crate::compose::filter_graph::ffmpeg_escape(&narration),
            voice
        );
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-f", "lavfi", "-i", &flite])
            .args([
                "-ar",
                "44100",
                "-ac",
                "2",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "128k",
            ])
            .arg(&dst)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| crate::error::KfError::Artifact(format!("spawn ffmpeg flite: {e}")))?;
        if !status.success() {
            return Err(crate::error::KfError::Artifact(format!(
                "ffmpeg flite exited {status:?} for narration: {narration}"
            )));
        }
    }
    Ok(script_path.to_string_lossy().into_owned())
}
fn scene_plan_with_brief(dir: &Path) -> serde_json::Value {
    let brief_path = dir.join("brief.txt");
    if brief_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&brief_path) {
            let parsed = brief::parse_brief(&text);
            return brief::scene_plan_from_brief(&parsed);
        }
    }
    brief::scene_plan_from_brief(&brief::Brief {
        title: "Pipeline Demo".into(),
        subtitle: Some("research → compose".into()),
        stats: vec![brief::Stat {
            number: "7".into(),
            label: "stages, fully resumable".into(),
            value: 1.0,
        }],
        captions: vec!["agentic".into(), "FFmpeg-native".into(), "Rust".into()],
        quotes: vec![],
        comparisons: vec![],
        callouts: vec![],
        end_tag: "kirkforge.video".into(),
    })
}
