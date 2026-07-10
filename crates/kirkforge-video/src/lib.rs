//! KirkForge Video — instruction-driven video production in Rust.
//! See ADR-001 for architecture.

pub mod compose;
pub mod demos;
pub mod error;
pub mod orchestrator;
pub mod pipelines;
pub mod tools;

pub use error::{KfError, Result};

use clap::Subcommand;
use std::path::PathBuf;

use crate::compose::scene_kind_tag;

#[derive(Subcommand, Debug, Clone)]
pub enum Cmd {
    /// Render a zero-key demo (e.g. world-in-numbers)
    Demo {
        name: String,
        #[arg(short, long, default_value = "out.mp4")]
        out: String,
    },
    /// List available demos and pipelines
    Demos,
    /// Run a pipeline end-to-end against a project directory
    Pipeline {
        kind: String,
        #[arg(short, long, default_value = "projects/default")]
        project: PathBuf,
        /// Optional text file. First line = title, next lines = captions.
        /// Stage::ScenePlan reads `<project>/brief.txt` if present.
        #[arg(long)]
        brief: Option<PathBuf>,
    },
    /// List and inspect built-in pipelines.
    Pipelines {
        #[command(subcommand)]
        op: PipelinesOp,
    },
    /// List and inspect built-in render profiles (for `kf render --profile`).
    Profiles {
        #[command(subcommand)]
        op: ProfilesOp,
    },
    /// Score a scene plan's slideshow risk
    Risk {
        kinds: Vec<String>,
        #[arg(long, default_value_t = 30.0)]
        duration_s: f32,
        /// Score this project's composition.json instead of passing kinds on the CLI
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Show version
    Version,
    /// List registered tools (or invoke one with --invoke + --params)
    Tools {
        #[arg(long)]
        invoke: Option<String>,
        #[arg(long, default_value = "{}")]
        params: String,
        /// Limit to one tier (core, provider, experimental)
        #[arg(long)]
        tier: Option<String>,
    },
    /// Re-run only the Compose stage against an existing scene_plan.json.
    /// ponytail: lets a user iterate on composition.json or scene_plan.json
    /// without replaying Research / Proposal / Script.
    Render {
        #[arg(short, long, default_value = "projects/default")]
        project: PathBuf,
        /// Render target profile (e.g. youtube_landscape, tiktok,
        /// youtube_shorts). Overrides composition width/height/fps.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Print recent entries from a project's decision_log.jsonl.
    DecisionLog {
        #[arg(short, long, default_value = "projects/default")]
        project: PathBuf,
        /// Only show decisions newer than this many seconds ago.
        #[arg(long)]
        since_s: Option<u64>,
        /// Filter by category (e.g. "slideshow_risk", "asset_transcode").
        #[arg(long)]
        category: Option<String>,
    },
    /// Run the full pipeline starting from a brief markdown file. Copies
    /// the brief to `<project>/brief.txt` and runs `kf pipeline`. ponytail:
    /// shorter form of `kf pipeline --brief <path>`.
    FromBrief {
        brief: PathBuf,
        #[arg(short, long, default_value = "projects/default")]
        project: PathBuf,
        #[arg(long, default_value = "animated_explainer")]
        kind: String,
    },
    /// Probe ffmpeg capabilities and report what's missing for KirkForge
    /// to render. Exits 0 if all checks pass, 1 if any fail.
    Doctor {
        #[command(subcommand)]
        op: DoctorOp,
    },
    /// Validate a scene_plan.json: parse it, build the filter graph, and
    /// report structural issues without rendering. ponytail: catches bad
    /// types, missing fields, zero-duration scenes, and slideshow risk
    /// before a 4-minute ffmpeg run fails.
    Validate {
        /// Path to scene_plan.json (or a directory containing artifacts/scene_plan.json).
        path: PathBuf,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum PipelinesOp {
    /// List available pipeline names (one per line: name + description).
    List,
    /// Print the stages of a single pipeline by name.
    Show { name: String },
}

#[derive(Subcommand, Debug, Clone)]
pub enum DoctorOp {
    /// Probe the ffmpeg binary for encoders + filters KirkForge needs.
    Ffmpeg {
        /// Path to the ffmpeg binary (default: `ffmpeg` from $PATH).
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg_path: String,
        /// Emit JSON instead of the human-readable PASS/FAIL list.
        #[arg(long)]
        json: bool,
    },
    /// Validate a project directory's files (brief.txt, brand.json,
    /// scene_plan.json, composition.json, risk_report.json,
    /// render/final.mp4). Missing optional files (brand, risk_report)
    /// are OK; missing required files FAIL.
    Project {
        /// Project directory (the one with brief.txt + artifacts/).
        #[arg(short, long, default_value = "projects/default")]
        project: PathBuf,
        /// Emit JSON instead of the human-readable PASS/FAIL list.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProfilesOp {
    /// List available render profiles (name + resolution + fps).
    List,
    /// Print the full spec for a single profile by name.
    Show { name: String },
}

pub async fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Version => {
            println!("kirkforge-video 0.1.0");
        }
        Cmd::Demos => {
            println!("Demos:");
            for d in demos::list() {
                println!("  {} — {}", d.label, d.description);
            }
            println!("\nPipelines:");
            for k in pipelines::Kind::all() {
                println!("  {}", k.label());
            }
        }
        Cmd::Demo { name, out } => {
            let path = demos::render(&name, PathBuf::from(&out)).await?;
            println!("rendered: {}", path.display());
        }
        Cmd::Pipeline {
            kind,
            project,
            brief,
        } => {
            let k = pipelines::Kind::from_label(&kind)
                .ok_or_else(|| anyhow::anyhow!("unknown pipeline: {kind}"))?;
            if let Some(b) = brief {
                let dst = project.join("brief.txt");
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&b, &dst).map_err(|e| {
                    anyhow::anyhow!("copy brief {} → {}: {e}", b.display(), dst.display())
                })?;
                tracing::info!(brief = %dst.display(), "seeded brief");
            }
            let pipe = pipelines::get(k);
            let reg = tools::ToolRegistry::with_builtins();
            orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await?;
        }
        Cmd::Pipelines { op } => match op {
            PipelinesOp::List => {
                for p in pipelines::all_pipelines() {
                    println!("{}\n  {}", p.name(), p.description());
                }
            }
            PipelinesOp::Show { name } => {
                let pipes = pipelines::all_pipelines();
                match pipes.iter().find(|p| p.name() == name) {
                    Some(p) => {
                        println!("{} - {}", p.name(), p.description());
                        println!("stages ({}):", p.stages().len());
                        for s in p.stages() {
                            println!("  - {s:?}");
                        }
                    }
                    None => {
                        let available: Vec<&str> = pipes.iter().map(|p| p.name()).collect();
                        anyhow::bail!(
                            "unknown pipeline {name:?}; available: {}",
                            available.join(", ")
                        );
                    }
                }
            }
        },
        Cmd::Profiles { op } => match op {
            ProfilesOp::List => {
                for p in compose::ALL_PROFILES {
                    println!(
                        "{:18}  {}x{} @ {}fps  crf={}",
                        p.name, p.width, p.height, p.fps, p.crf
                    );
                }
            }
            ProfilesOp::Show { name } => match compose::get_profile(&name) {
                Some(p) => {
                    println!("{} - {}", p.name, p.notes);
                    println!("  resolution : {}x{}", p.width, p.height);
                    println!("  aspect     : {}", p.aspect_ratio.as_label());
                    println!("  fps        : {}", p.fps);
                    println!("  codec      : {} / {}", p.codec, p.audio_codec);
                    println!("  crf        : {}", p.crf);
                    println!("  pixel fmt  : {}", p.pixel_format);
                    if let Some(mb) = p.max_file_size_mb {
                        println!("  max size   : {mb:.0} MB");
                    }
                    if let Some(s) = p.max_duration_seconds {
                        println!("  max dur    : {s:.0}s");
                    }
                    println!("  captions   : {}", p.caption_format);
                }
                None => {
                    let available: Vec<&str> =
                        compose::ALL_PROFILES.iter().map(|p| p.name).collect();
                    anyhow::bail!(
                        "unknown profile {name:?}; available: {}",
                        available.join(", ")
                    );
                }
            },
        },
        Cmd::Risk {
            kinds,
            duration_s,
            project,
        } => {
            let (refs, dur) = if let Some(proj) = project {
                let comp_path = proj.join("artifacts").join("composition.json");
                let raw = std::fs::read_to_string(&comp_path)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", comp_path.display()))?;
                let comp: compose::Composition = serde_json::from_str(&raw)?;
                let refs: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
                let dur = comp.total_duration_s();
                (refs, dur)
            } else {
                let refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
                (refs, duration_s)
            };
            let report = orchestrator::slideshow_risk::score_slideshow_risk(&refs, dur);
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Tools {
            invoke,
            params,
            tier,
        } => {
            let reg = tools::ToolRegistry::with_builtins();
            if let Some(name) = invoke {
                let t = reg.require(&name).map_err(|e| anyhow::anyhow!("{e}"))?;
                let v: serde_json::Value = serde_json::from_str(&params)?;
                match t.invoke(&PathBuf::from("."), "", v).await {
                    Ok(out) => println!("{}", serde_json::to_string_pretty(&out)?),
                    Err(e) => println!("error: {e}"),
                }
            } else {
                for n in reg.names() {
                    if let Some(t) = reg.get(n) {
                        let want = tier.as_deref().map(|t| t.to_lowercase());
                        let got = format!("{:?}", t.tier()).to_lowercase();
                        if want.as_ref().map(|w| &got != w).unwrap_or(false) {
                            continue;
                        }
                        println!(
                            "{} [{:?}/{:?}] {}",
                            n,
                            t.tier(),
                            t.stability(),
                            t.capabilities().join(", ")
                        );
                    }
                }
            }
        }
        Cmd::Render { project, profile } => {
            // ponytail: just synthesize composition from scene_plan.json
            // and render. No earlier stages. Reads risk + writes report.
            // Anything that wants the full pipeline goes through
            // `kf pipeline ...`.
            use crate::orchestrator::slideshow_risk;
            let arts = project.join("artifacts");
            let plan = arts.join("scene_plan.json");
            if !plan.exists() {
                anyhow::bail!(
                    "{}: scene_plan.json missing — run `kf pipeline` first",
                    plan.display()
                );
            }
            let raw = std::fs::read_to_string(&plan)?;
            // Build a Composition by hand here so the test path stays
            // decoupled from AnimatedExplainer. Mirror the synthesis rule.
            let plan_v: serde_json::Value = serde_json::from_str(&raw)?;
            let mut comp = synthesize_from_plan(&plan_v)?;
            // ponytail: when a profile is given, override the synthesized
            // composition's resolution + fps before writing the artifact.
            // Scenes themselves are not scaled — the render filter graph
            // draws at whatever resolution it was authored at; profiles
            // are target shapes, not authoring constraints.
            if let Some(name) = profile.as_deref() {
                let p = compose::get_profile(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown profile {name:?}; available: {}",
                        compose::ALL_PROFILES
                            .iter()
                            .map(|p| p.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
                compose::apply_to_composition(p, &mut comp);
                tracing::info!(
                    profile = p.name,
                    w = p.width,
                    h = p.height,
                    fps = p.fps,
                    "applying media profile"
                );
            }
            let comp_path = arts.join("composition.json");
            std::fs::write(&comp_path, serde_json::to_string_pretty(&comp)?)?;
            let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
            let report = slideshow_risk::score_slideshow_risk(&kinds, comp.total_duration_s());
            std::fs::write(
                arts.join("risk_report.json"),
                serde_json::to_string_pretty(&report)?,
            )?;
            // Render.
            let out = project.join("render").join("final.mp4");
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            compose::render_composition(&comp, &out).await?;
            tracing::info!(stage = "Compose", "rendered: {}", out.display());
            println!("{}", serde_json::to_string_pretty(&report)?);
            println!("rendered: {}", out.display());
        }
        Cmd::DecisionLog {
            project,
            since_s,
            category,
        } => {
            // ponytail: decision_log.jsonl is append-only. Read tail, filter,
            // print. No mutation.
            let log = project.join("artifacts").join("decision_log.jsonl");
            if !log.exists() {
                anyhow::bail!(
                    "{}: no decision_log.jsonl (run pipeline first)",
                    log.display()
                );
            }
            let raw = std::fs::read_to_string(&log)?;
            let now_s: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for line in raw.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(want) = &category {
                    let want_v = serde_json::Value::String(want.clone());
                    if v["category"] != want_v {
                        continue;
                    }
                }
                if let Some(window) = since_s {
                    // ponytail: ts is "epoch:N" (best-effort, no chrono dep).
                    if let Some(ts) = v["ts"].as_u64() {
                        if now_s.saturating_sub(ts) > window {
                            continue;
                        }
                    }
                }
                println!("{}", serde_json::to_string(&v)?);
            }
        }
        Cmd::FromBrief {
            brief,
            project,
            kind,
        } => {
            // ponytail: just dispatch to the pipeline path with the brief
            // already copied in. If brief == <project>/brief.txt, skip the
            // copy (std::fs::copy is a self-truncating no-op on Linux).
            let dst = project.join("brief.txt");
            if brief != dst {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&brief, &dst).map_err(|e| {
                    anyhow::anyhow!("copy brief {} → {}: {e}", brief.display(), dst.display())
                })?;
            }
            let k = pipelines::Kind::from_label(&kind)
                .ok_or_else(|| anyhow::anyhow!("unknown pipeline: {kind}"))?;
            let pipe = pipelines::get(k);
            let reg = tools::ToolRegistry::with_builtins();
            orchestrator::run_pipeline(pipe.as_ref(), &project, &reg).await?;
        }
        Cmd::Doctor { op } => match op {
            DoctorOp::Ffmpeg { ffmpeg_path, json } => {
                // ponytail: shell out, parse, emit. Exit code matters —
                // CI can gate on it. Use std::process::exit because the
                // anyhow::Result<()> path here would always return Ok
                // and the runner would mask the failure.
                let r = tools::doctor::run_doctor(&ffmpeg_path);
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    print!("{}", tools::doctor::render_text_report(&r));
                }
                if !r.all_passed() {
                    std::process::exit(1);
                }
            }
            DoctorOp::Project { project, json } => {
                let r = tools::doctor::run_project_doctor(&project);
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    print!("{}", tools::doctor::render_text_report(&r));
                }
                if !r.all_passed() {
                    std::process::exit(1);
                }
            }
        },
        Cmd::Validate { path } => {
            // ponytail: resolve the path. Accept either the scene_plan.json
            // file directly, or a project directory containing
            // artifacts/scene_plan.json.
            let plan_path = if path.is_dir() {
                path.join("artifacts").join("scene_plan.json")
            } else {
                path.clone()
            };
            if !plan_path.exists() {
                anyhow::bail!("{}: scene_plan.json not found", plan_path.display());
            }
            let raw = std::fs::read_to_string(&plan_path)?;
            let plan_v: serde_json::Value = serde_json::from_str(&raw)?;
            // Synthesis surfaces most structural errors (bad type, missing
            // fields, non-finite durations). Walk the result and accumulate
            // any extra warnings the renderer would also flag.
            let mut issues: Vec<String> = Vec::new();
            let comp = match synthesize_from_plan(&plan_v) {
                Ok(c) => c,
                Err(e) => {
                    println!("INVALID: {e}");
                    std::process::exit(1);
                }
            };
            // ponytail: per-scene duration / type sanity. Cheap, runs before
            // any ffmpeg call.
            for (i, s) in comp.scenes.iter().enumerate() {
                let kind = scene_kind_tag(s);
                let dur = crate::compose::scene_duration_s(s);
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
            // ponytail: slideshow risk score so the operator sees the
            // variety check before committing to a render.
            use crate::orchestrator::slideshow_risk;
            let kinds: Vec<&str> = comp.scenes.iter().map(scene_kind_tag).collect();
            let risk = slideshow_risk::score_slideshow_risk(&kinds, comp.total_duration_s());
            // ponytail: actually build the filter graph. This catches
            // chain-join errors (the ones that took three days to find
            // in the showcase smoke render). `build_filter_graph` is
            // infallible today, but if it ever returns Result, the
            // shape is already wired in.
            let filter_plan =
                crate::compose::build_filter_graph(&comp.scenes, comp.width, comp.height, comp.fps);
            // ponytail: cheap structural lint on the generated filter.
            // The ffmpeg parser is silent about double semicolons until
            // it fails — find them here.
            if filter_plan.filter_complex.contains(";;") {
                issues.push("filter graph contains `;;` (double semicolon)".into());
            }
            // Report.
            println!("scene_plan: {}", plan_path.display());
            println!(
                "scenes:     {} ({:.1}s total, {}x{} @ {} fps)",
                comp.scenes.len(),
                comp.total_duration_s(),
                comp.width,
                comp.height,
                comp.fps
            );
            println!("kinds:      {}", kinds.join(", "));
            println!("risk:       {:.2} ({:?})", risk.average, risk.verdict);
            if issues.is_empty() {
                println!("status:     OK — filter graph builds cleanly");
                std::process::exit(0);
            } else {
                println!("status:     WARN — {} issue(s):", issues.len());
                for i in &issues {
                    println!("  - {i}");
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// ponytail: thin re-implementation of `synthesize_composition` so the
/// `kf render` path doesn't depend on AnimatedExplainer's private helpers.
/// Synthesize a `Composition` from a `scene_plan.json`-shaped JSON
/// value. Used by `kf validate`, the render path, and the screen-demo
/// pipeline. ponytail: kept here (not in `compose/`) so the lib root
/// remains the orchestrator entry point.
pub fn synthesize_from_plan(
    plan: &serde_json::Value,
) -> anyhow::Result<crate::compose::Composition> {
    use crate::compose::{AudioSpec, Composition, Scene};
    let mut scenes = Vec::new();
    for s in plan["scenes"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("scene_plan: missing scenes array"))?
    {
        let kind = s["type"].as_str().unwrap_or("");
        let dur = s["duration_s"].as_f64().unwrap_or(3.0) as f32;
        let scene = match kind {
            "hero_title" => Scene::HeroTitle {
                text: s["title"].as_str().unwrap_or("").into(),
                subtitle: s["subtitle"].as_str().map(String::from),
                duration_s: dur,
                shot: None,
            },
            "end_tag" => Scene::EndTag {
                text: s["title"].as_str().unwrap_or("").into(),
                duration_s: dur,
                shot: None,
            },
            "stat_card" => Scene::StatCard {
                number: s["number"].as_str().unwrap_or("").into(),
                label: s["label"].as_str().unwrap_or("").into(),
                duration_s: dur,
                shot: None,
            },
            "bar_chart" => Scene::BarChart {
                title: s["title"].as_str().unwrap_or("").into(),
                bars: s["bars"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| {
                                Some(crate::compose::Bar {
                                    label: b["label"].as_str()?.into(),
                                    value: b["value"].as_f64()? as f32,
                                    color: b["color"].as_str().unwrap_or("#ffcc00").into(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                duration_s: dur,
                shot: None,
            },
            "line_chart" => Scene::LineChart {
                title: s["title"].as_str().unwrap_or("").into(),
                x_labels: s["x_labels"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                series: s["series"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|sr| {
                                Some(crate::compose::LineSeries {
                                    label: sr["label"].as_str()?.into(),
                                    values: sr["values"]
                                        .as_array()?
                                        .iter()
                                        .filter_map(|n| n.as_f64().map(|f| f as f32))
                                        .collect(),
                                    color: sr["color"].as_str().map(String::from),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                duration_s: dur,
                shot: None,
            },
            "pie_chart" => Scene::PieChart {
                title: s["title"].as_str().unwrap_or("").into(),
                slices: s["slices"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|sl| {
                                Some(crate::compose::PieSlice {
                                    label: sl["label"].as_str()?.into(),
                                    percent: sl["percent"].as_f64()? as f32,
                                    color: sl["color"].as_str().map(String::from),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                duration_s: dur,
                shot: None,
            },
            "text_card" => Scene::TextCard {
                title: s["title"].as_str().unwrap_or("").into(),
                body: s["body"].as_str().unwrap_or("").into(),
                duration_s: dur,
                shot: None,
            },
            "terminal_scene" => {
                use crate::compose::TerminalStep;
                let steps = s["steps"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|st| {
                                let kind = st["kind"].as_str()?;
                                let out = match kind {
                                    "cmd" => TerminalStep::Cmd {
                                        text: st["text"].as_str()?.into(),
                                        type_speed: st["type_speed"].as_f64().unwrap_or(0.035)
                                            as f32,
                                        hold_s: st["hold_s"].as_f64().unwrap_or(0.3) as f32,
                                    },
                                    "out" => TerminalStep::Out {
                                        text: st["text"].as_str()?.into(),
                                        hold_s: st["hold_s"].as_f64().unwrap_or(0.6) as f32,
                                    },
                                    "pause" => TerminalStep::Pause {
                                        seconds: st["seconds"].as_f64()? as f32,
                                    },
                                    "pill" => TerminalStep::Pill {
                                        text: st["text"].as_str()?.into(),
                                        color: st["color"].as_str().map(String::from),
                                        hold_s: st["hold_s"].as_f64().unwrap_or(1.6) as f32,
                                    },
                                    _ => return None,
                                };
                                Some(out)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Scene::TerminalScene {
                    title: s["title"].as_str().map(String::from),
                    prompt: s["prompt"].as_str().unwrap_or("$ ").into(),
                    accent_color: s["accent_color"].as_str().map(String::from),
                    steps,
                    duration_s: dur,
                    shot: None,
                }
            }
            "caption_overlay" => Scene::CaptionOverlay {
                lines: s["lines"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                duration_s: dur,
                shot: None,
            },
            "quote_card" => Scene::QuoteCard {
                quote: s["quote"].as_str().unwrap_or("").into(),
                author: s["author"].as_str().map(String::from),
                source: s["source"].as_str().map(String::from),
                duration_s: dur,
                shot: None,
            },
            "comparison" => Scene::Comparison {
                title: s["title"].as_str().map(String::from),
                left_label: s["left_label"].as_str().unwrap_or("").into(),
                left_value: s["left_value"].as_str().unwrap_or("").into(),
                right_label: s["right_label"].as_str().unwrap_or("").into(),
                right_value: s["right_value"].as_str().unwrap_or("").into(),
                duration_s: dur,
                shot: None,
            },
            "progress_bar" => Scene::ProgressBar {
                title: s["title"].as_str().map(String::from),
                progress: s["progress"].as_f64().unwrap_or(0.0) as f32,
                label: s["label"].as_str().map(String::from),
                duration_s: dur,
                shot: None,
            },
            "callout" => Scene::Callout {
                title: s["title"].as_str().unwrap_or("").into(),
                body: s["body"].as_str().unwrap_or("").into(),
                kind: s["kind"].as_str().unwrap_or("tip").into(),
                duration_s: dur,
                shot: None,
            },
            "kpi_grid" => Scene::KpiGrid {
                title: s["title"].as_str().unwrap_or("").into(),
                cells: s["cells"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| {
                                Some(crate::compose::KpiCell {
                                    label: c["label"].as_str()?.into(),
                                    value: c["value"].as_str().unwrap_or("0").into(),
                                    change: c["change"].as_f64().map(|n| n as f32),
                                    suffix: c["suffix"].as_str().map(String::from),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                duration_s: dur,
                shot: None,
            },
            "clip_cut" => Scene::ClipCut {
                src: s["src"].as_str().unwrap_or("").into(),
                in_s: s["in_s"].as_f64().unwrap_or(0.0) as f32,
                out_s: s["out_s"].as_f64().unwrap_or(dur as f64) as f32,
                shot: None,
            },
            other => anyhow::bail!("unknown scene type: {other}"),
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

#[cfg(test)]
mod tests {
    use super::synthesize_from_plan;
    use serde_json::json;

    #[test]
    fn synthesize_from_plan_handles_every_documented_scene_type() {
        // ponytail: any new scene type that's added to the Scene enum
        // must also get an arm here, or `kf render` and `kf validate`
        // will silently reject all on-disk scene_plan.json files that
        // use it. This test enumerates every kind the renderer knows
        // about and confirms each one synthesizes without error.
        let kinds = [
            "hero_title",
            "stat_card",
            "bar_chart",
            "line_chart",
            "pie_chart",
            "text_card",
            "caption_overlay",
            "quote_card",
            "comparison",
            "progress_bar",
            "callout",
            "kpi_grid",
            "end_tag",
            "clip_cut",
            "terminal_scene",
        ];
        for k in kinds {
            let v = json!({
                "kind": "scene_plan",
                "scenes": [{"type": k, "duration_s": 1.0,
                    "title": "T", "subtitle": "S", "number": "1", "label": "L",
                    "body": "B", "lines": ["x"], "quote": "Q", "author": "A",
                    "source": "X", "left_label": "L", "left_value": "1",
                    "right_label": "R", "right_value": "2",
                    "progress": 0.5,
                    "kind_kind": "tip",
                    "cells": [{"label": "l", "value": "1"}],
                    "bars": [{"label": "b", "value": 0.5, "color": "#ffcc00"}],
                    "series": [{"label": "s", "values": [0.1, 0.2]}],
                    "slices": [{"label": "p", "percent": 100.0}],
                    "src": "/tmp/x.mp4", "in_s": 0.0, "out_s": 1.0,
                    "x_labels": ["a", "b"],
                }],
            });
            synthesize_from_plan(&v).unwrap_or_else(|e| panic!("{k}: {e}"));
        }
    }

    #[test]
    fn synthesize_from_plan_rejects_unknown_scene_type() {
        let v = json!({
            "kind": "scene_plan",
            "scenes": [{"type": "definitely_not_real", "duration_s": 1.0}],
        });
        let err = synthesize_from_plan(&v).unwrap_err().to_string();
        assert!(err.contains("unknown scene type"), "got: {err}");
        assert!(err.contains("definitely_not_real"), "got: {err}");
    }
}
