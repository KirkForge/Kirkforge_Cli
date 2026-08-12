# Lessons — WO 29.8 + 29.9 session

## What I learned about this codebase

- **The worktree was broken on arrival**: `Cargo.toml:73` and `Cargo.lock:3750`
  had unresolved diff3 merge markers (`<<<<<<< HEAD` / `|||||||` / `>>>>>>>`).
  The prior "fix: resolve broken merge-conflict markers" commit (7d9f71c)
  missed these two spots — likely because `git status` showed clean (the
  markers were the committed state, not a working-tree conflict). Running
  `git grep -nE '^(<<<<<<< |>>>>>>> |\|\|\|\|\|\|\| )'` from a fresh clone
  would have caught it. **Always run this grep before trusting a worktree.**

- **`kf-plugin` was already fully folded** but a test (`folded_plugin_identification`)
  still asserted `is_folded("kf-plugin") == false`. The loader's `FOLDED_PLUGINS`
  list had `("kf-plugin", "kf-plugin-tools")` since WO 29.1, but the test was
  never updated. Pre-existing bug — the test was wrong, not the loader. Fixed
  in this session.

- **`npm_bin_dirs()` serves TWO different layouts**: the deleted source-tree
  layout (`<repo>/npm/kf-plugin/node_modules/.bin`) AND the user data-dir
  layout (`~/.local/share/kf-code/npm/kf-plugin/node_modules/.bin`). The
  source-layout walk was dead after WO 29.9, but the data-dir path is still
  live — a user can install a Node-based plugin SDK into their data dir. Don't
  delete the data-dir path; only the source-layout walk was removed.

- **Three `bundled_*` tests in tests.rs were already effectively dead**:
  `bundled_stratum_mode_tool_executes_via_host` always early-returned (no
  `plugins/stratum/` ever existed in the repo — only `plugins/kf-plugin/`).
  The other two (`bundled_plugins_load_from_data_dir`,
  `bundled_plugin_tool_commands_exist_in_data_dir`) relied on the now-deleted
  `plugins/` tree. All three + the `copy_dir_all` helper were deleted.

- **`.github/workflows/release.yml` had a Node SDK packaging section** not
  mentioned in WO 29.9 R2 (which only called out ci.yml). The release workflow
  built, stripped, and packaged the Node SDK into release archives. This was
  scope creep but mandatory — the release would fail without removing it. Also
  removed the now-unused `actions/setup-node` step.

## What I tried that didn't work

- Tried to run `cargo test` early to check the `folded_plugin_identification`
  inconsistency, but the initial `cargo check` took ~10 minutes (cold build).
  Should have let the first check run in the background while reading code.

## What I'd do differently next time

- Run `git grep` for merge markers on worktree entry before any other work.
  The broken Cargo.toml cost ~10 min of a `cargo check` cycle to discover.
- When a WO says "verify zero grep hits", distinguish source-tree references
  (must go) from runtime path strings (may stay if the runtime still serves
  that path). The workorder's "zero hits" was about source-tree dependencies,
  not the literal substring.
