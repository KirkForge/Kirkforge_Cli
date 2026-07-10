# KirkForge-Video

Instruction-driven video production in Rust, with no Node/Python/browser dependencies. Built to match the feature set of OpenMontage via FFmpeg-native rendering.

## What it is

A single Rust crate/binary that turns a markdown brief into a rendered video:

```
research → proposal → script → scene plan → assets → edit → compose (render)
```

It also works stage-by-stage: validate a plan, score slideshow risk, probe FFmpeg, render a demo, or list built-in pipelines/profiles.

## Binary

The binary is named `kirkforge-video` so it can be installed alongside the main `kirkforge` CLI without collisions.

```bash
# Build all workspace binaries (release)
../../../scripts/build-all.sh --release

# Or build just the Rust workspace
cargo build --workspace --release

# Probe your FFmpeg
../../../target/release/kirkforge-video doctor ffmpeg

# Render a zero-key demo
../../../target/release/kirkforge-video demo world-in-numbers -o /tmp/out.mp4

# Run full pipeline from a brief
../../../target/release/kirkforge-video from-brief examples/brief-focusflow.md --project projects/focusflow

# Render a scene plan to final.mp4
../../../target/release/kirkforge-video render --project projects/focusflow --profile tiktok
```

See `ADR-001-kirkforge-video-architecture.md` for the full design record.

## KirkForge CLI plugin

KirkForge-Video ships as a KirkForge plugin. Drop it into the CLI's plugin directory to make video tools available inside `kirkforge run`:

```bash
mkdir -p ~/.local/share/kirkforge/plugins
cp -R plugin ~/.local/share/kirkforge/plugins/kirkforge-video
```

Then set in `~/.local/share/kirkforge/config.toml`:

```toml
max_plugin_trust = "shell"
```

Exposed tools:

| Tool | Purpose |
|------|---------|
| `video_demos` | list demos, pipelines, profiles, tools |
| `video_pipeline` | run a full pipeline from a brief or existing project |
| `video_render` | render `scene_plan.json` to `render/final.mp4` |
| `video_validate` | validate a scene plan without rendering |
| `video_doctor` | probe FFmpeg or validate project artifacts |
| `video_risk` | score slideshow risk |
| `video_decision_log` | read the project's decision log |

Example chat usage inside `kirkforge run`:

```text
User: make an animated explainer from examples/brief-focusflow.md
Assistant: video_pipeline {"kind": "animated_explainer", "project": "projects/focusflow", "brief": "examples/brief-focusflow.md"}
Assistant: video_render {"project": "projects/focusflow"}
```

See `plugin/README.md` for full install details and binary discovery rules.

## Dependencies

- Rust 1.70+
- FFmpeg (with libx264, aac, drawtext, drawbox, xfade)
- `bash` for the plugin tool scripts

No Node, no Python, no browser, no Remotion.

## Development

This crate is part of the KirkForge workspace. Build and test it from the workspace root:

```bash
cd ../../..
cargo build --workspace --release
cargo test -p kirkforge-video --lib
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT
