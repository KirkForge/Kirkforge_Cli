# ADR-059: Plugin hot-reload via file watcher

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugin management was reactive, not proactive. The `/plugins reload`
slash command rescans the plugins directory and reloads the registry —
but only when the user asks. A developer iterating on a plugin's
`kirkforge.toml` or tool scripts had to `/plugins reload` after every
change. There was no automatic reload on file change.

## Decision

1. **Add `notify-debouncer-mini` as a dep.** It bundles `notify` (the
   cross-platform file-system watcher) with a debounce layer. On
   Linux it uses inotify, macOS FSEvents, Windows
   ReadDirectoryChangesW — all pure Rust via the `notify` crate. The
   size impact is small (notify is ~50KB in a size-optimized binary).

2. **`spawn_plugin_watcher` in the loader.** Takes the plugins dir
   path + a `tokio::sync::mpsc::UnboundedSender<()>` reload channel.
   Watches the directory recursively with a 500ms debounce
   (coalescing editor multi-file saves). Only reacts to files that
   look like plugin assets (`kirkforge.toml`, `.sh`, `.js`, `.ts`,
   `.py`). On a relevant change, sends `()` on the reload channel.

3. **Wire into the TUI.** The TUI spawns the watcher at startup
   alongside the existing config SIGHUP watcher. The reload signal
   triggers the same path as `/plugins reload`: rebuild the
   registry, forward it to the executor via `plugin_reload_tx`.

4. **Headless mode.** The watcher is not spawned in headless
   (`kirkforge run --non-interactive`) mode by default — a short CI
   run doesn't need it, and a long-running daemon would opt in via a
   future `--watch-plugins` flag. The TUI always has it on.

5. **Test.** An integration test (marked `#[ignore]` as timing-sensitive)
   writes a manifest, starts the watcher, modifies the manifest, and
   asserts the reload signal fires within ~3s.

## Consequences

- A change to a plugin's `kirkforge.toml` (or tool/hook script) in the
  `plugins/` directory triggers an automatic registry reload in the
  TUI within ~1s (500ms debounce + reload time).
- The reload uses the same path as `/plugins reload` (no new reload logic).
- The watcher is for *development* (iterating on a plugin). In
  production (a user running released `kirkforge`), the plugins are
  stable and the watcher is wasted CPU — but notify on a single small
  directory is cheap (inotify on Linux, FSEvents on macOS, etc.).
- The watcher does NOT watch `node_modules/` or `target/` — only the
  `plugins/` directory and its subdirectories, filtered by extension.

## Notes

- The `ponytail:` annotations in the loader code are preserved.
- This is a P2 WO — developer ergonomics, not a correctness gap.