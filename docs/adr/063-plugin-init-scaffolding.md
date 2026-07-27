# ADR-063: Plugin init scaffolding command

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugins were hand-authored. A new plugin author had to manually create
a directory, write a `kirkforge.toml` manifest with the right schema,
write tool/hook scripts, make them executable, and add the plugin to
the config. There was no scaffolding command — the author had to read an
existing plugin's manifest to learn the schema.

## Decision

1. **`kirkforge plugin init <name>` CLI subcommand.** Scaffolds a new
   plugin directory at `plugins/<name>/` (or `--path <dir>/<name>/`)
   with:
   - `kirkforge.toml` — a minimal valid manifest with `name`,
     `version = "0.1.0"`, `description`, `trust = "read-only"`, one
     `[[capabilities]]` skill entry with a placeholder prompt, and
     commented-out examples for tool/hook/verifier capabilities.
   - `tools/` + `hooks/` directories (with `.gitkeep`).
   - `README.md` — a one-liner pointing to `kirkforge.toml` and the
     getting-started + signing steps.

2. **Default `trust = "read-only"`.** The safest default — a
   scaffolded plugin can only read files, not run shell commands. The
   author bumps it to `shell` or `network` when they add a tool that
   needs it. This is least-surprise: a copy-pasted scaffold can't
   accidentally run arbitrary shell.

3. **Validation round-trip.** The scaffolded manifest is valid out of
   the box — `kirkforge plugin validate <path>` passes immediately.

## Consequences

- New plugin authors have a "hello world" path: `kirkforge plugin init
  my-plugin`, edit the prompt, `kirkforge plugin enable my-plugin`,
  `kirkforge run`.
- The scaffolded manifest's commented-out examples show how to add
  tools/hooks/verifiers without requiring the author to read the docs.
- The default `read-only` trust means a scaffolded plugin is safe to
  enable and test before the author adds shell capabilities.
- No `.kirkforge.sig` is scaffolded — that's for the author to create
  with `minisign -S` after writing the plugin. The `README.md`
  documents the signing step.

## Notes

- The plugin name is validated against the same kebab-case rule as the
  manifest's `name` field.
- The `--path` flag overrides the default `plugins/` parent.
- Depends on WO 11.0 (the `kirkforge plugin` CLI subcommand).