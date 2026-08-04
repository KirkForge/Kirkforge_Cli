//! In-process Video tool wrappers.
//!
//! When the `video` feature is enabled, these structs implement the `Tool`
//! trait and call `kf_video` functions directly, eliminating subprocess
//! overhead. When the feature is off, the shell-plugin path
//! (`plugins/kf-video/tools/*.sh`) remains as fallback.

use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

fn success(content: String) -> ToolOutcome {
    ToolOutcome::Success { content }
}

fn error(message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::Error {
        message: message.into(),
    }
}

fn json_get_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn json_get_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn json_get_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn json_get_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

fn json_get_string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_path(p: &str) -> PathBuf {
    let expanded = shellexpand::tilde(p).to_string();
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

// ── video_demos ──────────────────────────────────────────────────────────

pub struct VideoDemos;

#[async_trait::async_trait]
impl Tool for VideoDemos {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_demos",
            description: "List demos, pipelines, render profiles, or internal tools available in kf-video.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["demos", "pipelines", "profiles", "tools"],
                        "description": "What catalog to list",
                        "default": "demos"
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let cmd = json_get_string(&args, "command").unwrap_or_else(|| "demos".into());
        match cmd.as_str() {
            "demos" => {
                let demos = kf_video::demos::list();
                let lines: Vec<String> = demos
                    .iter()
                    .map(|d| format!("{} — {}", d.label, d.description))
                    .collect();
                success(lines.join("\n"))
            }
            "pipelines" => {
                let pipes = kf_video::pipelines::all_pipelines();
                let lines: Vec<String> = pipes
                    .iter()
                    .map(|p| format!("{} — {}", p.name(), p.description()))
                    .collect();
                success(lines.join("\n"))
            }
            "profiles" => {
                use kf_video::compose::ALL_PROFILES;
                let lines: Vec<String> = ALL_PROFILES
                    .iter()
                    .map(|p| {
                        format!(
                            "{:18}  {}x{} @ {}fps  crf={}",
                            p.name, p.width, p.height, p.fps, p.crf
                        )
                    })
                    .collect();
                success(lines.join("\n"))
            }
            "tools" => {
                let reg = kf_video::tools::ToolRegistry::with_builtins();
                let lines: Vec<String> = reg
                    .names()
                    .iter()
                    .filter_map(|n| {
                        reg.get(n).map(|t| {
                            format!(
                                "{} [{:?}/{:?}] {}",
                                n,
                                t.tier(),
                                t.stability(),
                                t.capabilities().join(", ")
                            )
                        })
                    })
                    .collect();
                success(lines.join("\n"))
            }
            other => error(format!(
                "video_demos: unknown command '{other}' (use demos|pipelines|profiles|tools)"
            )),
        }
    }
}

// ── video_pipeline ────────────────────────────────────────────────────────

pub struct VideoPipeline;

#[async_trait::async_trait]
impl Tool for VideoPipeline {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_pipeline",
            description: "Run a full video pipeline (research → proposal → script → scene_plan → assets → edit → compose). If a brief path is given, it is copied into the project first.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "Pipeline kind (animated_explainer, cinematic, screen_demo)",
                        "default": "animated_explainer"
                    },
                    "project": {
                        "type": "string",
                        "description": "Project directory path (absolute or relative to CWD)",
                        "default": "projects/default"
                    },
                    "brief": {
                        "type": "string",
                        "description": "Optional markdown brief file to seed the project"
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let kind_str =
            json_get_string(&args, "kind").unwrap_or_else(|| "animated_explainer".into());
        let project_str =
            json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
        let brief_str = json_get_string(&args, "brief");

        let kind = match kf_video::pipelines::Kind::from_label(&kind_str) {
            Some(k) => k,
            None => {
                return error(format!(
                    "video_pipeline: unknown pipeline kind '{kind_str}'"
                ))
            }
        };

        let project = resolve_path(&project_str);
        if let Some(parent) = project.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let reg = kf_video::tools::ToolRegistry::with_builtins();
        let pipe = kf_video::pipelines::get(kind);

        if let Some(brief_str) = brief_str {
            let brief_path = resolve_path(&brief_str);
            let dst = project.join("brief.txt");
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::copy(&brief_path, &dst) {
                return error(format!(
                    "video_pipeline: copy brief {} → {}: {e}",
                    brief_path.display(),
                    dst.display()
                ));
            }
        }

        match kf_video::orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await {
            Ok(()) => success(format!(
                "pipeline '{}' completed for {}",
                kind_str,
                project.display()
            )),
            Err(e) => error(format!("video_pipeline: {e:#}")),
        }
    }
}

// ── video_render ───────────────────────────────────────────────────────────

pub struct VideoRender;

#[async_trait::async_trait]
impl Tool for VideoRender {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_render",
            description: "Render an existing scene_plan.json to render/final.mp4. Optionally override with a media profile (tiktok, youtube_shorts, etc.).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Project directory",
                        "default": "projects/default"
                    },
                    "profile": {
                        "type": "string",
                        "description": "Media profile name"
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let project_str =
            json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
        let profile_str = json_get_string(&args, "profile");
        let project = resolve_path(&project_str);

        let plan_path = project.join("artifacts").join("scene_plan.json");
        if !plan_path.exists() {
            return error(format!(
                "video_render: {} missing — run video_pipeline first",
                plan_path.display()
            ));
        }

        let raw = match std::fs::read_to_string(&plan_path) {
            Ok(r) => r,
            Err(e) => {
                return error(format!(
                    "video_render: cannot read {}: {e}",
                    plan_path.display()
                ))
            }
        };
        let plan_v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return error(format!("video_render: parse scene_plan.json: {e}")),
        };

        let mut comp = match kf_video::synthesize_from_plan(&plan_v) {
            Ok(c) => c,
            Err(e) => return error(format!("video_render: synthesize: {e:#}")),
        };

        if let Some(name) = profile_str.as_deref() {
            match kf_video::compose::get_profile(name) {
                Some(p) => {
                    kf_video::compose::apply_to_composition(p, &mut comp);
                }
                None => {
                    let available: Vec<&str> = kf_video::compose::ALL_PROFILES
                        .iter()
                        .map(|p| p.name)
                        .collect();
                    return error(format!(
                        "video_render: unknown profile '{name}'; available: {}",
                        available.join(", ")
                    ));
                }
            }
        }

        let arts = project.join("artifacts");
        if let Err(e) = std::fs::create_dir_all(&arts) {
            return error(format!("video_render: cannot create artifacts dir: {e}"));
        }
        let comp_path = arts.join("composition.json");
        if let Err(e) = std::fs::write(
            &comp_path,
            serde_json::to_string_pretty(&comp).unwrap_or_default(),
        ) {
            return error(format!("video_render: write composition.json: {e}"));
        }

        use kf_video::compose::scene_kind_tag;
        use kf_video::orchestrator::slideshow_risk;
        let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
        let report = slideshow_risk::score_slideshow_risk(&kinds, comp.total_duration_s());
        let risk_path = arts.join("risk_report.json");
        let _ = std::fs::write(
            &risk_path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        );

        let out = project.join("render").join("final.mp4");
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match kf_video::compose::render_composition(&comp, &out).await {
            Ok(()) => {
                let risk_json = serde_json::to_string_pretty(&report).unwrap_or_default();
                success(format!("rendered: {}\n{}", out.display(), risk_json))
            }
            Err(e) => error(format!("video_render: {e:#}")),
        }
    }
}

// ── video_validate ─────────────────────────────────────────────────────────

pub struct VideoValidate;

#[async_trait::async_trait]
impl Tool for VideoValidate {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_validate",
            description: "Validate a scene_plan.json and its filter graph without rendering. Accepts a scene_plan.json path or a project directory.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to scene_plan.json or a project directory containing artifacts/scene_plan.json",
                        "default": "projects/default"
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let path_str = json_get_string(&args, "path").unwrap_or_else(|| "projects/default".into());
        let path = resolve_path(&path_str);

        let plan_path = if path.is_dir() {
            path.join("artifacts").join("scene_plan.json")
        } else {
            path.clone()
        };

        if !plan_path.exists() {
            return error(format!("video_validate: {} not found", plan_path.display()));
        }

        let raw = match std::fs::read_to_string(&plan_path) {
            Ok(r) => r,
            Err(e) => {
                return error(format!(
                    "video_validate: cannot read {}: {e}",
                    plan_path.display()
                ))
            }
        };
        let plan_v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return error(format!("video_validate: parse scene_plan.json: {e}")),
        };

        let comp = match kf_video::synthesize_from_plan(&plan_v) {
            Ok(c) => c,
            Err(e) => return error(format!("video_validate: INVALID — {e:#}")),
        };

        use kf_video::compose::scene_kind_tag;
        let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
        use kf_video::orchestrator::slideshow_risk;
        let risk = slideshow_risk::score_slideshow_risk(&kinds, comp.total_duration_s());
        let filter_plan = kf_video::compose::build_filter_graph(
            &comp.scenes,
            comp.width,
            comp.height,
            comp.fps,
        );

        let mut issues: Vec<String> = Vec::new();
        for (i, s) in comp.scenes.iter().enumerate() {
            let kind = scene_kind_tag(s);
            let dur = kf_video::compose::scene_duration_s(s);
            if dur <= 0.0 || !dur.is_finite() {
                issues.push(format!(
                    "scene {i} ({kind}): duration_s={dur} (must be > 0 and finite)"
                ));
            }
            if dur > 60.0 {
                issues.push(format!(
                    "scene {i} ({kind}): duration_s={dur:.1}s is unusually long (>60s)"
                ));
            }
        }
        if comp.scenes.is_empty() {
            issues.push("scene_plan contains no scenes".into());
        }
        if filter_plan.filter_complex.contains(";;") {
            issues.push("filter graph contains `;;` (double semicolon)".into());
        }

        let status = if issues.is_empty() { "OK" } else { "WARN" };
        let issues_str = if issues.is_empty() {
            String::new()
        } else {
            format!(
                "\nissues:\n{}",
                issues
                    .iter()
                    .map(|i| format!("  - {i}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        success(format!(
            "scene_plan: {}\nscenes: {} ({:.1}s total, {}x{} @ {} fps)\nkinds: {}\nrisk: {:.2} ({:?})\nstatus: {}{}",
            plan_path.display(),
            comp.scenes.len(),
            comp.total_duration_s(),
            comp.width,
            comp.height,
            comp.fps,
            kinds.join(", "),
            risk.average,
            risk.verdict,
            status,
            issues_str,
        ))
    }
}

// ── video_from_brief ──────────────────────────────────────────────────────

pub struct VideoFromBrief;

#[async_trait::async_trait]
impl Tool for VideoFromBrief {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_from_brief",
            description:
                "Shorthand: copy a brief markdown file into the project and run the pipeline.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "brief": {
                        "type": "string",
                        "description": "Path to brief markdown file"
                    },
                    "project": {
                        "type": "string",
                        "description": "Project directory",
                        "default": "projects/default"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Pipeline kind",
                        "default": "animated_explainer"
                    }
                },
                "required": ["brief"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let brief_str = match json_get_string(&args, "brief") {
            Some(b) => b,
            None => return error("video_from_brief: missing required 'brief' field"),
        };
        let project_str =
            json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
        let kind_str =
            json_get_string(&args, "kind").unwrap_or_else(|| "animated_explainer".into());

        let kind = match kf_video::pipelines::Kind::from_label(&kind_str) {
            Some(k) => k,
            None => {
                return error(format!(
                    "video_from_brief: unknown pipeline kind '{kind_str}'"
                ))
            }
        };

        let project = resolve_path(&project_str);
        let brief_path = resolve_path(&brief_str);
        let dst = project.join("brief.txt");

        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if brief_path != dst {
            if let Err(e) = std::fs::copy(&brief_path, &dst) {
                return error(format!(
                    "video_from_brief: copy brief {} → {}: {e}",
                    brief_path.display(),
                    dst.display()
                ));
            }
        }

        let reg = kf_video::tools::ToolRegistry::with_builtins();
        let pipe = kf_video::pipelines::get(kind);

        match kf_video::orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await {
            Ok(()) => success(format!(
                "pipeline '{}' completed for {}",
                kind_str,
                project.display()
            )),
            Err(e) => error(format!("video_from_brief: {e:#}")),
        }
    }
}

// ── video_doctor ───────────────────────────────────────────────────────────

pub struct VideoDoctor;

#[async_trait::async_trait]
impl Tool for VideoDoctor {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_doctor",
            description: "Probe FFmpeg capabilities or validate a project directory's artifacts.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "check": {
                        "type": "string",
                        "enum": ["ffmpeg", "project"],
                        "description": "Which check to run",
                        "default": "ffmpeg"
                    },
                    "project": {
                        "type": "string",
                        "description": "Project directory for project check",
                        "default": "projects/default"
                    },
                    "ffmpeg_path": {
                        "type": "string",
                        "description": "Path to ffmpeg binary for ffmpeg check",
                        "default": "ffmpeg"
                    },
                    "json": {
                        "type": "boolean",
                        "description": "Emit JSON instead of human-readable text",
                        "default": false
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let check = json_get_string(&args, "check").unwrap_or_else(|| "ffmpeg".into());
        let json_out = json_get_bool(&args, "json");

        match check.as_str() {
            "ffmpeg" => {
                let ffmpeg_path =
                    json_get_string(&args, "ffmpeg_path").unwrap_or_else(|| "ffmpeg".into());
                let report = kf_video::tools::doctor::run_doctor(&ffmpeg_path);
                if json_out {
                    success(serde_json::to_string_pretty(&report).unwrap_or_default())
                } else {
                    success(kf_video::tools::doctor::render_text_report(&report))
                }
            }
            "project" => {
                let project_str =
                    json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
                let project = resolve_path(&project_str);
                let report = kf_video::tools::doctor::run_project_doctor(&project);
                if json_out {
                    success(serde_json::to_string_pretty(&report).unwrap_or_default())
                } else {
                    success(kf_video::tools::doctor::render_text_report(&report))
                }
            }
            other => error(format!(
                "video_doctor: unknown check '{other}' (use ffmpeg|project)"
            )),
        }
    }
}

// ── video_risk ─────────────────────────────────────────────────────────────

pub struct VideoRisk;

#[async_trait::async_trait]
impl Tool for VideoRisk {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_risk",
            description: "Score slideshow risk for a scene plan. Pass a project directory (reads composition.json) or a list of scene kinds + duration.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Project directory containing artifacts/composition.json"
                    },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Scene kind tags (ignored if project is given)"
                    },
                    "duration_s": {
                        "type": "number",
                        "description": "Duration in seconds when using kinds",
                        "default": 30
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let project_str = json_get_string(&args, "project");
        let duration_s = json_get_f64(&args, "duration_s").unwrap_or(30.0) as f32;

        let report = if let Some(proj) = &project_str {
            let project = resolve_path(proj);
            let comp_path = project.join("artifacts").join("composition.json");
            let raw = match std::fs::read_to_string(&comp_path) {
                Ok(r) => r,
                Err(e) => return error(format!("video_risk: {}: {e}", comp_path.display())),
            };
            let comp: kf_video::compose::Composition = match serde_json::from_str(&raw) {
                Ok(c) => c,
                Err(e) => return error(format!("video_risk: parse composition.json: {e}")),
            };
            use kf_video::compose::scene_kind_tag;
            let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
            kf_video::orchestrator::slideshow_risk::score_slideshow_risk(
                &kinds,
                comp.total_duration_s(),
            )
        } else {
            let kinds = json_get_string_array(&args, "kinds");
            if kinds.is_empty() {
                return error("video_risk: provide project or kinds array");
            }
            let kinds_refs: Vec<&str> = kinds.iter().map(|s| s.as_str()).collect();
            kf_video::orchestrator::slideshow_risk::score_slideshow_risk(
                &kinds_refs,
                duration_s,
            )
        };

        success(serde_json::to_string_pretty(&report).unwrap_or_default())
    }
}

// ── video_decision_log ────────────────────────────────────────────────────

pub struct VideoDecisionLog;

#[async_trait::async_trait]
impl Tool for VideoDecisionLog {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "video_decision_log",
            description: "Print recent entries from a project's decision_log.jsonl.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Project directory",
                        "default": "projects/default"
                    },
                    "since_s": {
                        "type": "integer",
                        "description": "Only show entries newer than this many seconds ago"
                    },
                    "category": {
                        "type": "string",
                        "description": "Filter by category (e.g. slideshow_risk, asset_transcode)"
                    }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let project_str =
            json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
        let since_s = json_get_u64(&args, "since_s");
        let category = json_get_string(&args, "category");

        let project = resolve_path(&project_str);
        let log_path = project.join("artifacts").join("decision_log.jsonl");

        if !log_path.exists() {
            return error(format!(
                "video_decision_log: {} not found (run video_pipeline first)",
                log_path.display()
            ));
        }

        let raw = match std::fs::read_to_string(&log_path) {
            Ok(r) => r,
            Err(e) => {
                return error(format!(
                    "video_decision_log: cannot read {}: {e}",
                    log_path.display()
                ))
            }
        };

        let now_s: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut entries = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(ref cat) = category {
                let want_v = Value::String(cat.clone());
                if v["category"] != want_v {
                    continue;
                }
            }
            if let Some(window) = since_s {
                if let Some(ts) = v["ts"].as_u64() {
                    if now_s.saturating_sub(ts) > window {
                        continue;
                    }
                }
            }
            entries.push(v);
        }

        if entries.is_empty() {
            success("(no matching entries)".into())
        } else {
            let lines: Vec<String> = entries
                .iter()
                .filter_map(|v| serde_json::to_string(v).ok())
                .collect();
            success(lines.join("\n"))
        }
    }
}

/// Return all eight video tools as trait objects.
pub fn video_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(VideoDemos),
        Arc::new(VideoPipeline),
        Arc::new(VideoRender),
        Arc::new(VideoValidate),
        Arc::new(VideoFromBrief),
        Arc::new(VideoDoctor),
        Arc::new(VideoRisk),
        Arc::new(VideoDecisionLog),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    #[tokio::test]
    async fn test_video_demos_returns_output() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"command": "demos"}))
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    !content.is_empty(),
                    "VideoDemos must return a non-empty listing"
                );
                assert!(
                    content.contains("world-in-numbers") || content.contains(" — "),
                    "VideoDemos output must list a demo, got: {content}"
                );
            }
            other => panic!("VideoDemos must return Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_validate_returns_output() {
        let tool = VideoValidate;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"path": "/nonexistent/kf-video-test-scene-plan.json"}),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    !content.is_empty(),
                    "VideoValidate Success must be non-empty"
                );
            }
            ToolOutcome::Error { message } => {
                assert!(
                    !message.is_empty(),
                    "VideoValidate Error must carry a message"
                );
                assert!(
                    message.contains("not found") || message.contains("video_validate"),
                    "VideoValidate error should mention the missing path, got: {message}"
                );
            }
            other => panic!("VideoValidate must return Success or Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_demos_unknown_command_errors() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"command": "bogus"}))
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("unknown command") && message.contains("bogus"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_demos_lists_pipelines() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"command": "pipelines"}))
            .await;
        match out {
            ToolOutcome::Success { content } => assert!(!content.is_empty(), "got: {content}"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_demos_lists_profiles() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"command": "profiles"}))
            .await;
        match out {
            ToolOutcome::Success { content } => assert!(!content.is_empty(), "got: {content}"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_demos_lists_tools() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"command": "tools"}))
            .await;
        match out {
            ToolOutcome::Success { content } => assert!(!content.is_empty(), "got: {content}"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_demos_defaults_to_demos_command() {
        let tool = VideoDemos;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        assert!(matches!(out, ToolOutcome::Success { .. }));
    }

    #[tokio::test]
    async fn test_video_pipeline_unknown_kind_errors() {
        let tool = VideoPipeline;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"kind": "definitely-not-a-pipeline"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("unknown pipeline kind"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_render_missing_plan_errors() {
        let tool = VideoRender;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"project": "/nonexistent/kf-code-render-test"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("missing") && message.contains("scene_plan.json"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_render_unknown_profile_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().to_path_buf();
        let arts = project.join("artifacts");
        std::fs::create_dir_all(&arts).unwrap();
        let plan_path = arts.join("scene_plan.json");
        std::fs::write(&plan_path, r#"{"scenes": []}"#).unwrap();
        let tool = VideoRender;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"project": project, "profile": "not-a-profile"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("unknown profile") && message.contains("not-a-profile"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_validate_missing_path_errors() {
        let tool = VideoValidate;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"path": "/nonexistent/kf-code-validate-test.json"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("not found"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_from_brief_missing_brief_errors() {
        let tool = VideoFromBrief;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("missing required") && message.contains("brief"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_from_brief_unknown_kind_errors() {
        let tool = VideoFromBrief;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"brief": "/tmp/none.txt", "kind": "no-such-kind"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("unknown pipeline kind"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_from_brief_unreadable_brief_errors() {
        let tool = VideoFromBrief;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"brief": "/nonexistent/kf-code-brief.md"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("copy brief") || message.contains("video_from_brief"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_doctor_unknown_check_errors() {
        let tool = VideoDoctor;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"check": "mystery"}))
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("unknown check") && message.contains("mystery"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_doctor_ffmpeg_default_runs() {
        let tool = VideoDoctor;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        assert!(matches!(out, ToolOutcome::Success { .. }));
    }

    #[tokio::test]
    async fn test_video_doctor_project_returns_report() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = VideoDoctor;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"check": "project", "project": tmp.path()}),
            )
            .await;
        assert!(matches!(out, ToolOutcome::Success { .. }));
    }

    #[tokio::test]
    async fn test_video_risk_kinds_returns_report() {
        let tool = VideoRisk;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"kinds": ["text", "image", "text"], "duration_s": 15.0}),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => assert!(
                !content.is_empty() && (content.contains("average") || content.contains("verdict")),
                "got: {content}"
            ),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_risk_no_input_errors() {
        let tool = VideoRisk;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Error { message } => assert!(
                message.contains("project") || message.contains("kinds"),
                "got: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_risk_missing_composition_errors() {
        let tool = VideoRisk;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"project": "/nonexistent/kf-code-risk-test"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("composition.json"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_decision_log_missing_file_errors() {
        let tool = VideoDecisionLog;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"project": "/nonexistent/kf-code-decision-test"}),
            )
            .await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("not found"), "got: {message}")
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_decision_log_empty_returns_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let arts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&arts).unwrap();
        std::fs::write(arts.join("decision_log.jsonl"), "").unwrap();
        let tool = VideoDecisionLog;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"project": tmp.path()}))
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("no matching entries"), "got: {content}")
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_decision_log_returns_matching_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let arts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&arts).unwrap();
        let entry = serde_json::json!({
            "category": "slideshow_risk",
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            "message": "test-entry",
        });
        std::fs::write(
            arts.join("decision_log.jsonl"),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let tool = VideoDecisionLog;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"project": tmp.path()}))
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("test-entry"), "got: {content}")
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_decision_log_filters_by_category() {
        let tmp = tempfile::tempdir().unwrap();
        let arts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&arts).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a = serde_json::json!({"category": "alpha", "ts": now, "msg": "a"});
        let b = serde_json::json!({"category": "beta", "ts": now, "msg": "b"});
        std::fs::write(
            arts.join("decision_log.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&a).unwrap(),
                serde_json::to_string(&b).unwrap()
            ),
        )
        .unwrap();
        let tool = VideoDecisionLog;
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"project": tmp.path(), "category": "alpha"}),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("\"alpha\""), "got: {content}");
                assert!(!content.contains("\"beta\""), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_video_tools_returns_all_tools() {
        let tools = video_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"video_demos"), "{names:?}");
        assert!(names.contains(&"video_pipeline"), "{names:?}");
        assert!(names.contains(&"video_render"), "{names:?}");
        assert!(names.contains(&"video_validate"), "{names:?}");
        assert!(names.contains(&"video_from_brief"), "{names:?}");
        assert!(names.contains(&"video_doctor"), "{names:?}");
        assert!(names.contains(&"video_risk"), "{names:?}");
        assert!(names.contains(&"video_decision_log"), "{names:?}");
    }

    #[test]
    fn test_json_helpers_round_trip() {
        let args = serde_json::json!({
            "s": "value",
            "n": 42u64,
            "b": true,
            "f": 3.14_f64,
            "arr": ["a", "b", "c"],
        });
        assert_eq!(json_get_string(&args, "s"), Some("value".to_string()));
        assert_eq!(json_get_u64(&args, "n"), Some(42));
        assert!(json_get_bool(&args, "b"));
        assert!(!json_get_bool(&args, "missing"));
        assert_eq!(json_get_f64(&args, "f"), Some(3.14));
        assert_eq!(
            json_get_string_array(&args, "arr"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert!(json_get_string_array(&args, "missing").is_empty());
    }

    #[test]
    fn test_resolve_path_expands_tilde_and_relative() {
        let abs = resolve_path("/tmp/kf-code-test-abs");
        assert!(abs.is_absolute(), "got: {abs:?}");
        let rel = resolve_path("kf-code-test-relative");
        assert!(
            rel.is_absolute(),
            "relative should be made absolute: {rel:?}"
        );
        let home = resolve_path("~/kf-code-test-tilde");
        assert!(home.is_absolute(), "tilde should be expanded: {home:?}");
    }
}
