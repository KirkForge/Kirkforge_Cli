# KirkForge-Video plugin for KirkForge-Cli

This directory is a KirkForge plugin that exposes the `kirkforge-video` pipeline as tools inside the `kirkforge` TUI/CLI. The binary builds from `crates/kirkforge-video` in this workspace.

## Install

1. Build the `kirkforge-video` binary from this workspace:
   ```bash
   cargo build --workspace --release
   ```
   The plugin tool scripts prefer `target/release/kirkforge-video` and fall back to `kirkforge-video` on `PATH`.

2. Copy this directory into the KirkForge plugins folder:
   ```bash
   mkdir -p ~/.local/share/kirkforge/plugins
   cp -R plugins/kirkforge-video ~/.local/share/kirkforge/plugins/kirkforge-video
   ```

3. Set `max_plugin_trust = "shell"` (or higher) in `~/.local/share/kirkforge/config.toml`, because the plugin shells out to `kirkforge-video` and FFmpeg.

4. Restart `kirkforge run`. The TUI status bar should show the video tools loaded.

## Tools exposed

| Tool name | What it calls | Typical args |
|-----------|---------------|--------------|
| `video_demos` | `kirkforge-video demos` (or pipelines/profiles/tools) | `{"command": "pipelines"}` |
| `video_pipeline` | `kirkforge-video from-brief ...` or `pipeline ...` | `{"kind": "animated_explainer", "project": "projects/default", "brief": "briefs/focusflow.md"}` |
| `video_render` | `kirkforge-video render ...` | `{"project": "projects/default", "profile": "tiktok"}` |
| `video_validate` | `kirkforge-video validate ...` | `{"path": "projects/default"}` |
| `video_doctor` | `kirkforge-video doctor ffmpeg` or `doctor project` | `{"check": "ffmpeg"}` or `{"check": "project", "project": "projects/default"}` |
| `video_risk` | `kirkforge-video risk ...` | `{"project": "projects/default"}` or `{"kinds": ["hero_title", "stat_card"], "duration_s": 30}` |
| `video_decision_log` | `kirkforge-video decision-log ...` | `{"project": "projects/default", "since_s": 3600}` |

All arguments are passed via the `KIRKFORGE_TOOL_ARGS_JSON` env var as JSON. Tools write their results to stdout.

## Example chat turns

```text
User: make a 30-second animated explainer from examples/brief-focusflow.md
Assistant: video_pipeline {"kind": "animated_explainer", "project": "projects/focusflow", "brief": "examples/brief-focusflow.md"}
Assistant: video_render {"project": "projects/focusflow"}
```

## Binary discovery

The shell tools look for `kirkforge-video` in this order:

1. `../../../target/release/kirkforge-video` and `../../../target/debug/kirkforge-video` (workspace-built binary).
2. Next to the script itself (for local development).
3. Any `kirkforge-video` on `PATH`.

If you installed the binary somewhere else, add it to `PATH` or symlink it into `~/.cargo/bin`.

## Trust tier

The manifest declares `trust = "shell"`. The plugin does not execute arbitrary user commands, but it does spawn the `kirkforge-video` binary and FFmpeg subprocesses. Do not raise `max_plugin_trust` above what you need.

## License

MIT — same as the rest of KirkForge-Video.
