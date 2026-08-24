# ADR-050: Plugin System Consolidation

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-24

## Context

Workorders 6.6–6.9 folded four satellite plugins (Stratum, Plugin3, Draw,
Video) into the core binary behind feature flags. Each fold-in registered
in-process Rust tools in `main/mod.rs` and in-process hooks in the executor.
However, the shell-plugin loader (`load_workspace_plugins`) continued to load
the same plugin names from `plugins/*/kf-code.toml`, causing duplicate
tool registrations. The `CompositeToolset` deduplicates by first-set-wins, so
the shell "plugin" toolset (added first) shadowed the in-process toolset.

The plugin system now has two dispatch paths:

1. **Compiled-in** (feature on): tools register as direct Rust calls in
   `main/mod.rs`; hooks register as `InProcessHook` handlers in the executor.
   The shell plugin dir is skipped by the loader.

2. **External** (feature off): the shell plugin dir loads via
   `PluginToolWrapper` shell-outs (graceful degradation). The in-process
   tools and hooks are not registered.

The `kf-plugin-sdk` self-plugin (Node SDK) remains an external shell-out
under all configurations. Its 6 tools (`plugin_verify`,
`plugin_verify_workspace`, `plugin_audit_verify`, `plugin_doctor`,
`plugin_health`, `plugin_tools`) shell out to a Node CLI that probes for
ESLint, TypeScript, Ruff, Pyright, and Bandit. These are deterministic checks
that depend on the Node ecosystem and external linters; porting them to Rust
would require reimplementing or shelling out to those tools anyway. The
decision is to keep them as shell-outs with the Node SDK as an optional
external dependency.

## Decision

1. **Two-path dispatch**: the loader (`load_workspace_plugins`) checks
   `folded_feature_enabled(name)` for each enabled plugin. When a folded
   plugin's feature is ON, the shell plugin dir is skipped — the in-process
   version is the sole provider. When OFF, the shell plugin dir loads as
   fallback.

2. **Graceful degradation**: a user who builds without `--features video`
   still gets video support via the shell plugin (if the plugin dir and
    `kf-video` binary are available). A user who builds with
   `--features video` gets the compiled-in version with no subprocess
   overhead.

3. **Single toggle**: `enabled_plugins` in `ToolConfig` controls both
   paths. A folded plugin name in `enabled_plugins` enables the
   compiled-in path (feature on) or the shell path (feature off).
   `plugin_sources` is only needed for external/shell plugins.

4. **Node SDK**: kept as external shell-out. The `kf-plugin-sdk` plugin
   is not folded; its tools depend on the Node ecosystem (ESLint,
   TypeScript, Ruff, Pyright, Bandit). Porting to Rust would not eliminate
   the external dependency on those tools.

5. **`/plugins list`**: shows source (`compiled-in` / `external` /
   `external (feature off)`) and feature gate for each workspace plugin
   source.

## Consequences

- No duplicate tool registrations: when a feature is on, only the
  compiled-in tools register; the shell plugin is skipped.
- Users can mix: e.g., build with `--features stratum,budget` for
  compiled-in Stratum+Plugin3, while Draw and Video run as shell plugins.
- The Node SDK remains an optional external dependency; users who don't
  need verification tools can disable the `kf-plugin-sdk` plugin.
- `is_folded()` and `folded_feature()` are public so the TUI and tests
  can query the fold-in status.

## Implementation notes

- `FOLDED_PLUGINS` const maps plugin names to feature names.
- `folded_feature_enabled(name)` uses `#[cfg(feature = "...")]` match arms
  — compile-time, not runtime.
- The `default_plugin_sources` and `default_enabled_plugins` functions in
  `ToolConfig` already handle `#[cfg(feature = "video")]` gating for the
  video plugin source. Other folded plugins (stratum, draw, budget) are
  always in the defaults; their feature gating happens in the loader, not
  the config.
- 3 new tests: `default_plugin_sources_are_present_and_loadable` (updated
  to assert folded plugins are NOT shell-loaded when feature is on),
  `folded_plugin_shell_fallback_when_feature_off`, and
  `folded_plugin_identification`.

## Update (WO 11.1, ADR-057)

The signature verification backend changed from a `minisign` binary
shell-out to an in-process pure-Rust verify via the `minisign-verify`
crate. The `verify_signatures` / `signature_key_path` config fields and
the `TrustPolicy::with_verify_signatures` API are unchanged; only the
backend changed. See ADR-057 for the decision and dep choice.

## Amendment (2026-08-22, WO 41.3) — Node SDK + shell fallbacks deleted

Several statements above describe paths that no longer exist:

- The "Node SDK" paragraph (`kf-plugin-sdk` self-plugin as an external
  shell-out with 6 tools `plugin_verify`, etc.) is **stale**: the
  `npm/kf-plugin/` tree was deleted in WO 29.9. The six tools are now
  compiled-in Rust impls behind the `kf-plugin-tools` feature (WO 29.1);
  `verify` runs the security emitter natively, `audit_verify` walks the
  hash chain natively (WO 35.6), and `verify_workspace` reports
  not-implemented (the crate reducer shipped in WO 37.2/ADR-076; this tool
  is not yet wired to it).
- The "Graceful degradation" clause stating "a user who builds without
  `--features video` still gets video support via the shell plugin" and the
  reference to `plugins/*/kf-code.toml` fallbacks are **stale**: the shell
  plugin trees were deleted in WO 29.9. Building without a feature means
  the plugin's capabilities are absent, not shell-backed. ADR-048 (Draw)
  and ADR-049 (Video) are Superseded.
- The `plugins/stratum/` shell fallback referenced in the Implementation
  notes is likewise gone (see ADR-046 amendment).

The two-path dispatch (compiled-in vs absent) survives, but the "external
shell-out" path is gone. See `docs/TECHNICAL.md` § Plugin system for the
current model.