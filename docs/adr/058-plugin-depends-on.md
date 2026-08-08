# ADR-058: Plugin manifest `depends_on` (dependency graph)

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

A plugin manifest (`kf-code.toml`) declared `name`, `version`,
`description`, `trust`, `api_version`, `capabilities`, `metadata` —
but no `depends_on` field. There was no way for plugin B to declare "I
require plugin A to be loaded first."

Real example: the `stratum` plugin's `session-start` hook emits the
active compression ruleset; the `kf-plugin` (budget) plugin's
hooks read the ruleset to decide when to compact. WO 8.6 wired the
Stratum+Budget coordination at the executor level — but the manifest
had no way to declare the dependency. If a user enables
`kf-plugin` without `stratum`, the budget hooks run with no
ruleset and silently degrade.

## Decision

1. **Add `depends_on: Vec<String>` to `PluginManifest`** with
   `#[serde(default, rename = "depends_on")]`. The per-field rename
   keeps the snake_case TOML key (`depends_on = [...]`) even though the
   struct uses `rename_all = "kebab-case"` — the WO spec and the real
   manifests use `depends_on`, not `depends-on`. Existing manifests
   without the field parse unchanged (defaults to empty).

2. **Validate `depends_on` names.** In `PluginManifest::validate()`,
   each entry must be a valid plugin name (same kebab-case regex as the
   `name` field), non-empty, and not equal to the plugin's own name
   (no self-dependency).

3. **Topological load order.** `PluginRegistry::load_from_dir` now
   collects all plugins first, runs a DFS-based topological sort over
   the `depends_on` graph, and indexes plugins in dependency order:
   dependencies before dependents. A missing dependency (not in the
   loaded set) produces a clear error naming the missing plugin. A
   cycle (A → B → A) produces a cycle-path error.

4. **Real dependency: plugin3 → stratum.** `plugins/kf-plugin/kf-code.toml`
   now declares `depends_on = ["stratum"]`, making the implicit WO 8.6
   dependency explicit in the manifest.

## Consequences

- Plugin dependencies are now declared, not implicit. The loader
  enforces them and orders hooks so dependencies fire first.
- Backward compatible: existing manifests without `depends_on` parse
  and load unchanged (empty list, no ordering change).
- A plugin with a missing dependency is rejected with a clear error
  naming the missing plugin (not a silent degradation).
- A dependency cycle is rejected with the cycle path.
- The TOML key is `depends_on` (snake_case), not `depends-on` (kebab-
  case) — the per-field `rename` override keeps it consistent with the
  WO spec and the existing `depends_on` convention in the codebase.

## Notes

- The `depends_on` field is a list of *plugin names*, not feature flags.
- The executor-level WO 8.6 Stratum+Budget coordination stays; `depends_on`
  makes the dependency explicit so the loader can enforce it.
- `ponytail:` annotations in the plugin manifest code are preserved.