# ADR-060: Per-plugin resource limits (extend SandboxConfig to plugin tools)

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

WO 9.8 shipped `SandboxConfig` with rlimits (`RLIMIT_CPU`, `RLIMIT_AS`,
`RLIMIT_FSIZE`) applied to the bash tool's child shell. But the plugin
tool path (`PluginToolWrapper`) spawned plugin tool scripts via
`Command::new(script_path)` with no resource limits. A plugin tool
script (e.g. a `video_pipeline` shell-out that invokes FFmpeg) could
run forever, allocate unbounded memory, or write unbounded files —
the same risks WO 9.8 closed for the bash tool.

## Decision

1. **Extend `SandboxConfig` to `PluginToolWrapper`.** The wrapper gains
   a `sandbox: SandboxConfig` field. In `run`, before spawning the
   plugin script, `setup_rlimits` (the same `pre_exec` hook from
   WO 9.8, now `pub(crate)`) is applied. On Windows this is a no-op
   with the same one-shot warning as the bash tool.

2. **Per-manifest `resource_limits` overrides.** `PluginManifest` gains
   an optional `resource_limits: Option<ResourceLimits>` field (serde
   `#[serde(rename = "resource_limits")]` to keep the snake_case TOML
   key despite the struct-level `rename_all = "kebab-case"`). The
   `ResourceLimits` struct has three optional fields: `cpu_secs`,
   `memory_mb`, `filesize_mb`.

3. **`SandboxConfig::merge_with`.** A new method produces a per-plugin
   config by overlaying the manifest's `resource_limits` on the global
   default: each `Some` field overrides the global; `None` fields fall
   back to the global. The `harden` flag is inherited from the global
   (a per-plugin manifest cannot disable hardening — only raise limits).

4. **Trust-tier gating.** Only apply rlimits when the plugin's
   effective trust is `Shell` or higher (a `ReadOnly` plugin tool
   doesn't spawn a subprocess — it's a skill prompt). This is already
   enforced by `SandboxPolicy::required_tier` (tool = Shell), so the
   rlimit path only runs for shell-tier tools.

## Consequences

- Plugin tools now have the same rlimit sandbox as the bash tool when
  `harden` is true.
- A heavy plugin (e.g. video with FFmpeg) can declare a higher memory
  limit than the global default via `resource_limits`, while a light
  plugin uses the default.
- The global `SandboxConfig` default applies to all plugins that don't
  declare `resource_limits` (least-surprise).
- The Windows no-op path matches WO 9.8's: one-shot `OnceLock` warning,
  not a crash, not a silent no-op.
- The `#[ignore]` test `plugin_tool_resource_limit_kills_cpu_burn_with_sigxcpu`
  proves the rlimit fires (requires a real CPU burn, too slow for CI).

## Notes

- The `ponytail:` annotations in `src/session/bash_runner/mod.rs` (the
  rlimit seam) and in `crates/kirkforge-plugin-host/src/sandbox.rs` are
  preserved.
- rlimits are NOT applied to `ReadOnly` plugin tools (skills) — they
  don't spawn subprocesses. The trust-tier gating handles this.
- The `setup_rlimits` function was changed from `fn` to `pub(crate) fn`
  so the plugin wrapper can reuse it — no behavior change.