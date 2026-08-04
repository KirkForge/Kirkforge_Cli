# ADR-001: KirkForge-Video Architecture

**Status:** Accepted
**Date:** 2026-07-04 (updated 2026-07-05)
**Context:** Match the features of OpenMontage-main in Rust, single-binary, no JS/Node dependency.

## What we're building

An instruction-driven video production CLI in Rust. Agent (human or LLM) drives a
pipeline of stages (`research → proposal → script → scene_plan → assets → edit → compose`)
through a `Tool` trait abstraction. Composes with FFmpeg directly (no Remotion).

## OpenMontage feature parity

| OpenMontage feature | Rust equivalent |
|---|---|
| `BaseTool` Python ABC | `Tool` trait (sync + async) in `src/tools/mod.rs` |
| `tools/video/video_stitch.py` (FFmpeg stitch + crossfade + spatial) | `tools::video::VideoStitch` — calls `ffmpeg` via `tokio::process` |
| `tools/audio/audio_mixer.py` (mix/duck/fade/normalize) | `tools::audio::AudioMixer` — FFmpeg `amerge`/`sidechaincompress` |
| `pipeline_defs/*.yaml` (animated-explainer, avatar-spokesperson, …) | `pipelines/` — serde_yaml structs, same stage list (3 built-in: animated_explainer, cinematic, screen_demo) |
| `lib/checkpoint.py` (stage resume) | `orchestrator::Checkpoint` — JSON state file per project |
| `lib/delivery_promise.py` (PromiseType enum) | `orchestrator::PromiseType` — strum enum, same 8 variants |
| `lib/slideshow_risk.py` (6-dim scoring) | `orchestrator::slideshow_risk` — pure fn, no Python deps, takes views-aware `SceneView { kind, motion }` |
| `lib/variation_checker.py` (8-check plan validation) | `orchestrator::variation_checker` — repeated shot size, static overuse, hero moment, generic phrases |
| `remotion-composer/` (React scene library: text_card, stat_card, chart, kpi_grid, comparison, progress_bar, callout, quote_card, caption_overlay, line_chart, pie_chart) | `compose::Scene` tagged enum, 13 variants — all rendered via FFmpeg `drawtext` + `drawbox` + `geq` (for the pie) + `xfade`, no JS |
| `lib/media_profiles.py` (9 platform presets) | `compose::media_profiles` — youtube_landscape, youtube_4k, youtube_shorts, instagram_reels, instagram_feed, tiktok, linkedin, cinematic, generic_hd |
| `style/playbook` palette + accent | `compose::brand::BrandTheme` — reads `<project>/brand.json` if present; primary_color threads through StatCard / QuoteCard author / EndTag / ProgressBar fill / KpiGrid value |
| `render_demo.py` (3 zero-key demos) | `examples/demo_*` + `demos::` registry — JSON props → composition JSON |
| `decision_log` entries | `orchestrator::DecisionLog` — append-only JSONL |
| `tools/analysis/*` (scene_detect, audio_probe, transcriber) | `tools::analysis` — wrappers around FFmpeg/Whisper CLI |
| FFmpeg capability probe (env validator) | `tools::doctor::run_doctor` — parses `ffmpeg -version` / `-encoders` / `-filters`; column-agnostic parser handles the encoder (7-char) vs filter (5-char) flag prefixes |
| Project file validator | `tools::doctor::run_project_doctor` — checks brief.txt, brand.json, scene_plan.json, composition.json, risk_report.json, render/final.mp4 |
| `tools/cost_tracker.py` | `kf tools list` — registry introspection |
| Pipeline / profile catalog (`list_pipelines`, `get_pipeline_spec`) | `kf pipelines list\|show <name>` + `kf profiles list\|show <name>` |

## Pipeline stages (mirrors OpenMontage)

```
research → proposal → script → scene_plan → assets → edit → compose
                                       ↑
                              checkpoint between every stage
```

Each stage reads the previous stage's artifact from disk, writes its own,
records a checkpoint, and either proceeds or asks for human approval.

## Composition model (FFmpeg-native, no JS)

Scenes are described in a typed DSL:

```rust
enum Scene {
    HeroTitle     { text, subtitle?, duration_s, shot? },
    TextCard      { title, body, duration_s, shot? },
    StatCard      { number, label, duration_s, shot? },
    BarChart      { title, bars: Vec<Bar>, duration_s, shot? },
    LineChart     { title, x_labels, series: Vec<LineSeries>, duration_s, shot? },
    PieChart      { title, slices: Vec<PieSlice>, duration_s, shot? },
    CaptionOverlay{ lines: Vec<String>, duration_s, shot? },
    QuoteCard     { quote, author?, source?, duration_s, shot? },
    Comparison    { title?, left_label, left_value, right_label, right_value, duration_s, shot? },
    ProgressBar   { title?, progress: f32, label?, duration_s, shot? },
    Callout       { title, body, kind: tip|warning|info, duration_s, shot? },
    KpiGrid       { title, cells: Vec<KpiCell>, duration_s, shot? },
    EndTag        { text, duration_s, shot? },
    ClipCut       { src: PathBuf, in_s, out_s, shot? },
}
```

Every scene carries an optional `ShotMeta { shot_type, camera_motion, narrative_role, transition }`
so slideshow risk + variation checker can score motion / size repetition without rerunning the render.

A `Composer` renders a `Vec<Scene>` to FFmpeg filter graphs:
- Text → `drawtext=` with system font (auto-escapes `:`, `'`, `%`)
- Charts → `drawbox` for bars + KPI cells (no Cairo, no JS)
- LineChart → per-series `drawbox` polyline segments (thickness=3) + baseline/top rule `drawbox`s
- PieChart → `geq` filter computing per-pixel polar angle + slice membership, then `overlay` back onto the bg; legend on the right
- Comparison → vertical divider `drawbox` + dual `drawtext`
- ProgressBar → track `drawbox` + brand-primary fill `drawbox`
- Callout → 8-px accent `drawbox` + title/body `drawtext`
- KpiGrid → ceil(√n) cols of value/label/arrow `drawtext`s
- Transitions → `xfade` filter (kinds: fade, wipeleft, wiperight, slideup, slidedown, circleopen, dissolve)
- Captions → SRT sidecar + `subtitles=` filter muxed as `mov_text`
- Camera motion → `scale=...:crop=...` (push 1.15×, pan, tilt, dolly, fade)

## Brief parser

Markdown-ish brief → scene plan:

```text
FocusFlow                          # hero title
Distraction-free deep work         # subtitle (line 2)
- 3.2x output per hour             # stat_card
- -47% context switches            # stat_card
> Less is more. — Dieter Rams      # quote_card (em-dash)
> 32s :: 12s                       # comparison (::)
> 60s :: 8s :: Build time          # comparison with title
> focusflow.app                    # end_tag (plain >, no em-dash)
```

Numeric items also feed a synthesized bar chart (top 4 by magnitude).
Non-numeric items become caption overlays, grouped 3-per-scene.

## Non-goals (this iteration)

- AI video generation providers (Veo, Runway, Pika) — provider trait stub only
- HyperFrames/Remotion parity — out of scope; we render in FFmpeg
- Hyperreal avatar / face-rig — stub the tool, real impl is a future ADR
- Live web UI — CLI + JSON artifacts only

## Crate layout

```
kirkforge-video/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI entry, clap subcommands
│   ├── lib.rs               # re-exports + Cmd enum + synthesize_from_plan
│   ├── tools/
│   │   ├── mod.rs           # Tool trait, ToolRegistry, ToolResult
│   │   ├── video.rs         # VideoStitch (concat/xfade/spatial/PiP)
│   │   ├── audio.rs         # AudioMixer (mix/duck/fade/normalize)
│   │   ├── analysis.rs      # scene_detect, audio_probe, frame_sampler
│   │   ├── enhancement.rs   # color_grade, upscale stubs
│   │   ├── transcoder.rs    # FFmpeg h264 yuv420p transcode
│   │   ├── providers.rs     # AI provider stubs
│   │   └── doctor.rs        # ffmpeg capability probe + project validator
│   ├── pipelines/
│   │   ├── mod.rs           # Pipeline trait, Kind enum, all_pipelines()
│   │   ├── animated_explainer.rs # full 8-stage pipeline
│   │   ├── cinematic.rs     # stub
│   │   ├── screen_demo.rs   # stub
│   │   └── brief.rs         # brief.txt → scene_plan parser
│   ├── compose/
│   │   ├── mod.rs           # Scene enum, Composition, scene_kind_tag, scene_duration_s, caption_overlay_srt
│   │   ├── filter_graph.rs  # FFmpeg filter builder (build_filter_graph_with_brand)
│   │   ├── render.rs        # execute ffmpeg
│   │   ├── brand.rs         # BrandTheme (palette + primary_color)
│   │   └── media_profiles.rs # 9 platform render profiles
│   ├── orchestrator/
│   │   ├── mod.rs           # run_pipeline(pipeline, project_dir)
│   │   ├── checkpoint.rs    # read/write stage state
│   │   ├── decision.rs      # DecisionLog
│   │   ├── slideshow_risk.rs # 6-dim scoring
│   │   └── variation_checker.rs # 8-check plan validation
│   ├── demos/
│   └── error.rs             # thiserror, single KfError enum
├── tests/integration.rs     # end-to-end render tests against real ffmpeg
└── examples/
    ├── brief-code-to-screen.md
    ├── brief-focusflow.md
    └── brief-world-in-numbers.md
```

## Dependencies (minimal)

- `tokio` (async runtime, process)
- `serde` + `serde_json` + `serde_yaml`
- `clap` (CLI)
- `thiserror` + `anyhow`
- `tracing` + `tracing-subscriber`
- `strum` (enum iteration)
- `which` (locate ffmpeg/ffprobe)
- `tempfile` (project workspaces)

FFmpeg is the only external binary dep. No Node, no Python, no browser.

## Verification

```sh
cargo build
cargo test --lib               # 83 lib tests (scene rendering, brief parser,
                               # brand theme, media profiles, slideshow risk,
                               # variation checker, doctor, pipelines, brief,
                               # line chart, pie chart, hex parser)
cargo test --test integration  # end-to-end ffmpeg render tests
cargo run -- doctor ffmpeg           # probe encoders + filters
cargo run -- doctor project --project projects/focusflow
cargo run -- demo world-in-numbers -o out.mp4
ffprobe out.mp4                # expect 1920x1080, h264, ~30s
cargo run -- pipelines list    # catalog of built-in pipelines
cargo run -- profiles list     # catalog of media profiles
cargo run -- render --profile tiktok --project projects/focusflow
```