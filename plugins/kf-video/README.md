# Kf-Code-Video plugin for Kf-Code-Cli

This directory is a Kf-Code plugin that exposes the `kf-code-video` pipeline as tools inside the `kf-code` TUI/CLI. The binary builds from `crates/kf-code-video` in this workspace.

## Install

1. Build the `kf-code-video` binary from this workspace:
   ```bash
   cargo build --workspace --release
   ```
   The plugin tool scripts prefer `target/release/kf-code-video` and fall back to `kf-code-video` on `PATH`.

2. Copy this directory into the Kf-Code plugins folder:
   ```bash
   mkdir -p ~/.local/share/kf-code/plugins
   cp -R plugins/kf-code-video ~/.local/share/kf-code/plugins/kf-code-video
   ```

3. Set `max_plugin_trust = "shell"` (or higher) in `~/.local/share/kf-code/config.toml`, because the plugin shells out to `kf-code-video` and FFmpeg.

4. Restart `kf-code run`. The TUI status bar should show the video tools loaded.

## Tools exposed

| Tool name | What it calls | Typical args |
|-----------|---------------|--------------|
| `video_demos` | `kf-code-video demos` (or pipelines/profiles/tools) | `{"command": "pipelines"}` |
| `video_pipeline` | `kf-code-video from-brief ...` or `pipeline ...` | `{"kind": "animated_explainer", "project": "projects/default", "brief": "briefs/focusflow.md"}` |
| `video_render` | `kf-code-video render ...` | `{"project": "projects/default", "profile": "tiktok"}` |
| `video_validate` | `kf-code-video validate ...` | `{"path": "projects/default"}` |
| `video_doctor` | `kf-code-video doctor ffmpeg` or `doctor project` | `{"check": "ffmpeg"}` or `{"check": "project", "project": "projects/default"}` |
| `video_risk` | `kf-code-video risk ...` | `{"project": "projects/default"}` or `{"kinds": ["hero_title", "stat_card"], "duration_s": 30}` |
| `video_decision_log` | `kf-code-video decision-log ...` | `{"project": "projects/default", "since_s": 3600}` |

All arguments are passed via the `KIRKFORGE_TOOL_ARGS_JSON` env var as JSON. Tools write their results to stdout.

## Example chat turns

```text
User: make a 30-second animated explainer from examples/brief-focusflow.md
Assistant: video_pipeline {"kind": "animated_explainer", "project": "projects/focusflow", "brief": "examples/brief-focusflow.md"}
Assistant: video_render {"project": "projects/focusflow"}
```

## Binary discovery

The shell tools look for `kf-code-video` in this order:

1. `../../../target/release/kf-code-video` and `../../../target/debug/kf-code-video` (workspace-built binary).
2. Next to the script itself (for local development).
3. Any `kf-code-video` on `PATH`.

If you installed the binary somewhere else, add it to `PATH` or symlink it into `~/.cargo/bin`.

## Trust tier

The manifest declares `trust = "shell"`. The plugin does not execute arbitrary user commands, but it does spawn the `kf-code-video` binary and FFmpeg subprocesses. Do not raise `max_plugin_trust` above what you need.

## License

MIT — same as the rest of Kf-Code-Video.
