# ADR-073: Break the tools↔session Circular Dependency (Layering Ports)

- **Status:** Accepted (partially implemented)
- **Date:** 2026-08-12

## Context

`src/tools/` reached UP into `src/session/` for the access guard (`PathGuard`,
`DenyList`, `GuardVerdict`), bash command-string safety (`check_bash_command_str`),
the undo enum (`UndoKind`), the toolset composition types, and — worst of all —
the nested `Executor` itself (`InProcessTaskSpawner` lived in `tools/task.rs` and
constructed the very loop that calls the tool). WO 28.1 (finding H7) recorded the
cycle gitnexus confirmed:

```
tools/mod → session::toolset → tools::Tool            (the real production cycle)
tools/*   → session::access                            (24 one-way upward edges)
```

The 24 `tools → session::access` edges are one-way (access does not import tools),
but they block extracting `tools` to its own crate (`kf-tools`): the dependency
arrow points the wrong way. `tools` should depend on `shared`, not `session`.

## Decision

Relocate the pure, session-state-free types down into `shared/`, and move the
`Executor`-constructing spawner up into `session/`. Four moves:

1. **access → `shared::access`** — `PathGuard`, `DenyList`, `GuardVerdict`,
   `ReadGate`, `deny_list`, `access_from_config`, and the multi-layer checks.
   `session::access` becomes a `pub use crate::shared::access;` re-export shim so
   non-tool callers (session internals, tui, main, jobs) keep resolving.
2. **bash safety → `shared::bash_safety`** — `check_bash_command_str` and its
   evasion helpers (pure static analysis). `session::bash_runner` re-exports them.
3. **`UndoKind` → `shared::undo`** — pure enum; `session::undo` re-exports.
4. **toolset → `tools::toolset`** — `Toolset`/`VecToolset`/`CompositeToolset` are
   the tools' own composition primitives. `session::toolset` becomes the assembly
   re-export. This is the move that actually cuts the production cycle.
5. **`InProcessTaskSpawner` → `session::task_spawner`** — the concrete impl that
   builds the nested `Executor`. The `TaskSpawner` **port trait stays in
   `tools::task`** (it already existed); tools call the trait, session provides the
   impl. This is the single intentional seam where the loop plugs into the tool.

### Why relocation instead of a `Guard` port trait (WO 28.1 R1 as written)

WO 28.1 R1 prescribed a `tool::Guard` port trait so `PathGuard` could stay in
`session`. On inspection, `access/mod.rs` is **fully pure** (its only production
crate-level dependency is `shared::Config`; the one `session::config` reference is
test-only and was relocated with its test). A within-crate relocation is therefore:

- **lower risk** than trait-ifying 14 tool constructors + ~80 test construction
  sites (compiler-verified import-path moves vs. logic rewrites), and
- **better layering** for the stated goal: pure types belong in `shared`, the
  lowest layer, which is where `check_bash_command_str` (R2) had to move anyway.

A port trait for `Guard` is also YAGNI today: there is exactly one implementation
(`PathGuard`), so a trait adds a dynamic-dispatch seam with no second consumer.
The trait can be introduced later if a second guard impl appears. This is the
deferral this ADR pins.

### Residual `tools → session` edges (3, all non-cyclic)

`grep "use crate::session::" src/tools/` drops from **26 production lines** to
**3**, each a genuine session-layer concern that needs its own port trait to cut:

| Residual | Why it stays |
|----------|-------------|
| `bash::bash_runner::{is_timeout_marker, run_shell_with_token, ShellError}` | Shell process I/O — needs a `ShellRunner` port (new WO). |
| `bash::bash_jobs::global_registry` | Process-global job registry; depends on `session::process_group`. |
| `remember::memory::{slugify_description, MemoryStore}` | WO 29.6 memory-palace; needs a `MemoryStore` port. |

None of these imports `tools`, so none forms a tools↔session cycle. WO 28.1's "≤2"
target slightly undercounted `bash.rs` (it imports four symbols from `bash_runner`;
only `check_bash_command_str` was movable). The gitnexus-flagged cycle through
`access` + `toolset` is fully cut.

## Consequences

- `tools` now depends on `shared` (correct direction) for access/safety/undo/toolset
  types, unblocking a future `kf-tools` crate extraction.
- The `session::access`, `session::toolset`, `session::undo`, and
  `session::bash_runner` re-exports are compatibility shims; a follow-up can repoint
  their callers at `shared`/`tools` and delete the shims.
- `TaskSpawner` (in `tools::task`) + `InProcessTaskSpawner` (in `session`) is the
  deliberate inversion seam: the tool depends on the port, the session wires the impl.
- The `Guard`/`DenyList` polymorphism deferral is tracked here; revisit if a second
  guard/deny implementation is needed.
