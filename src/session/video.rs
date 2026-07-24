//! In-process Video tool wrappers.
//!
//! When the `video` feature is enabled, these structs implement the `Tool`
//! trait and call `kirkforge_video` functions directly, eliminating subprocess
//! overhead. When the feature is off, the shell-plugin path
//! (`plugins/kirkforge-video/tools/*.sh`) remains as fallback.

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
            description: "List demos, pipelines, render profiles, or internal tools available in kirkforge-video.",
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
                let demos = kirkforge_video::demos::list();
                let lines: Vec<String> = demos
                    .iter()
                    .map(|d| format!("{} — {}", d.label, d.description))
                    .collect();
                success(lines.join("\n"))
            }
            "pipelines" => {
                let pipes = kirkforge_video::pipelines::all_pipelines();
                let lines: Vec<String> = pipes
                    .iter()
                    .map(|p| format!("{} — {}", p.name(), p.description()))
                    .collect();
                success(lines.join("\n"))
            }
            "profiles" => {
                use kirkforge_video::compose::ALL_PROFILES;
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
                let reg = kirkforge_video::tools::ToolRegistry::with_builtins();
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

        let kind = match kirkforge_video::pipelines::Kind::from_label(&kind_str) {
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

        let reg = kirkforge_video::tools::ToolRegistry::with_builtins();
        let pipe = kirkforge_video::pipelines::get(kind);

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

        match kirkforge_video::orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await {
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

        let mut comp = match kirkforge_video::synthesize_from_plan(&plan_v) {
            Ok(c) => c,
            Err(e) => return error(format!("video_render: synthesize: {e:#}")),
        };

        if let Some(name) = profile_str.as_deref() {
            match kirkforge_video::compose::get_profile(name) {
                Some(p) => {
                    kirkforge_video::compose::apply_to_composition(p, &mut comp);
                }
                None => {
                    let available: Vec<&str> = kirkforge_video::compose::ALL_PROFILES
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

        use kirkforge_video::compose::scene_kind_tag;
        use kirkforge_video::orchestrator::slideshow_risk;
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

        match kirkforge_video::compose::render_composition(&comp, &out).await {
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

        let comp = match kirkforge_video::synthesize_from_plan(&plan_v) {
            Ok(c) => c,
            Err(e) => return error(format!("video_validate: INVALID — {e:#}")),
        };

        use kirkforge_video::compose::scene_kind_tag;
        let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
        use kirkforge_video::orchestrator::slideshow_risk;
        let risk = slideshow_risk::score_slideshow_risk(&kinds, comp.total_duration_s());
        let filter_plan = kirkforge_video::compose::build_filter_graph(
            &comp.scenes,
            comp.width,
            comp.height,
            comp.fps,
        );

        let mut issues: Vec<String> = Vec::new();
        for (i, s) in comp.scenes.iter().enumerate() {
            let kind = scene_kind_tag(s);
            let dur = kirkforge_video::compose::scene_duration_s(s);
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

        let kind = match kirkforge_video::pipelines::Kind::from_label(&kind_str) {
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

        let reg = kirkforge_video::tools::ToolRegistry::with_builtins();
        let pipe = kirkforge_video::pipelines::get(kind);

        match kirkforge_video::orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await {
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
                let report = kirkforge_video::tools::doctor::run_doctor(&ffmpeg_path);
                if json_out {
                    success(serde_json::to_string_pretty(&report).unwrap_or_default())
                } else {
                    success(kirkforge_video::tools::doctor::render_text_report(&report))
                }
            }
            "project" => {
                let project_str =
                    json_get_string(&args, "project").unwrap_or_else(|| "projects/default".into());
                let project = resolve_path(&project_str);
                let report = kirkforge_video::tools::doctor::run_project_doctor(&project);
                if json_out {
                    success(serde_json::to_string_pretty(&report).unwrap_or_default())
                } else {
                    success(kirkforge_video::tools::doctor::render_text_report(&report))
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
            let comp: kirkforge_video::compose::Composition = match serde_json::from_str(&raw) {
                Ok(c) => c,
                Err(e) => return error(format!("video_risk: parse composition.json: {e}")),
            };
            use kirkforge_video::compose::scene_kind_tag;
            let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
            kirkforge_video::orchestrator::slideshow_risk::score_slideshow_risk(
                &kinds,
                comp.total_duration_s(),
            )
        } else {
            let kinds = json_get_string_array(&args, "kinds");
            if kinds.is_empty() {
                return error("video_risk: provide project or kinds array");
            }
            let kinds_refs: Vec<&str> = kinds.iter().map(|s| s.as_str()).collect();
            kirkforge_video::orchestrator::slideshow_risk::score_slideshow_risk(
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
