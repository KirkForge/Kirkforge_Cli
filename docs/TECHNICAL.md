# KirkForge Architecture

KirkForge is a provider-agnostic, verification-first coding agent. It combines
semantic code understanding, token-budget management, context compression, and
deterministic verification into a single Rust binary with an interactive TUI.
Specialized runtimes for diagram rendering and instruction-driven video editing
ship as satellite binaries orchestrated through the plugin system.

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
| Context cost (output side) | Plugin3 budget guard tracks token spend against a ceiling and slices or compacts oversized tool results when the budget is approached. |
| Execution reliability | A verifier bus runs build, test, lint, rustfmt, git-state, and security checks after file-modifying tool calls. A correction loop auto-applies formatter fixes and feeds unfixable errors back to the model as tool results. |
| Reproducibility | Enforced plan mode (`/plan` then `/implement`), per-result checkpointing mid-batch, execution replay (ADR-039), and conversation logging. |
| Extensibility | A manifest-based plugin system (`kf-code.toml`) with trust tiers, minisign signature verification, and four capability kinds: skills, tools, hooks, verifiers. |

---

## Workspace layout

The workspace has one binary crate (`kf-code`) and 16 satellite crates under
`crates/`. The binary is the user-facing CLI; the satellites are libraries and
standalone binaries.

```
kf-code (root bin)          ← the CLI the user runs
├── src/                       ← agent core (session, tools, TUI, adapters, verifiers)
├── crates/                    ← 16 satellite crates
│   ├── kf-plugin-sdk     ← plugin SDK: manifest types, trust tiers
│   ├── kf-plugin-host  ← plugin runtime: registry, dispatch, signatures
│   ├── kf-context-index← tree-sitter symbol/import/call-graph index
│   ├── kf-workflow     ← programmable JSON workflow engine
│   ├── kf-lsp          ← LSP client pool for symbol-aware navigation
│   ├── kf-bench        ← task-benchmark harness (types + verifier + reports)
│   ├── kf-draw-core    ← pure document model for KirkForge-Draw
│   ├── kf-draw         ← kfd: terminal diagram editor binary
│   ├── kf-video        ← instruction-driven video production binary
│   ├── kf-compress-core       ← context-compression pipeline library
│   ├── kf-compress-hosts      ← host-specific compression rules
│   ├── kf-compress-cli        ← stratum: compression CLI binary
│   ├── kf-budget-core           ← budget/orchestrator/slicing data model
│   ├── kf-budget-hosts          ← host-side budget adapters
│   ├── kf-budget-cli            ← kf-budget: budget CLI binary
│   └── kf-testdoctor   ← test-performance doctor (workspace member; profile, profile-per-test, classify, partition, suggest, suggest-detailed, apply, gaps, diagnose, flaky)
├── plugins/                   ← 5 plugin manifests + shell tool/hook scripts
│   ├── kf-plugin/      ← SDK self-plugin (Node-backed verification tools)
│   ├── stratum/               ← compression plugin (5 tools, 2 hooks)
│   ├── kf-budget/     ← budget plugin (7 tools, 4 hooks)
│   ├── kf-draw/        ← diagram plugin (1 tool, 1 hook)
│   └── kf-video/       ← video plugin (8 tools)
├── benches/tasks/             ← 31 benchmark task definitions (TOML)
└── docs/adr/                  ← 84 Architecture Decision Records
```

The workspace has ~3,900 `#[test]` functions (~2,200 under `src/`,
~1,650 under `crates/`). The `crates/` count is pinned by the
`readme_drift` test (`crates/kf-budget-core/README.md` State table).

### Compiled-in vs satellite

The root `kf-code` binary directly depends on six crates:

| Crate | Role |
|---|---|
| `kf-plugin-sdk` | Plugin manifest types and trust-tier logic |
| `kf-plugin-host` | Plugin registry, dispatch, in-process signature verification (ADR-057) |
| `kf-context-index` | Tree-sitter indexing and graph retrieval |
| `kf-workflow` | JSON workflow engine (reuses the `task` tool's spawner) |
| `kf-lsp` | LSP client pool |
| `kf-bench` | Benchmark task types, loader, verifier, report writers |

The remaining nine crates are **satellites**: they build as standalone binaries
(`kfd`, `kf-video`, `stratum`, `kf-budget`) or support libraries. When
their feature flag is enabled, the core crate is linked directly into the
`kf-code` binary as a compiled-in module (ADR-046–049). When the feature is
off, the shell plugin dir loads as a fallback (ADR-050).

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
  shell script as a subprocess). Folded plugins (Stratum, Plugin3, Draw, Video)
  register as direct Rust `Tool` impls when their feature is on (ADR-050).
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

19 tools implementing the `Tool` trait: `read_file`, `write_file`, `edit_file`,
`atomic_write`, `bash`, `bash_cancel`, `bash_minify`, `bash_status`, `glob`,
`grep`, `lsp_query`, `read_image`, `web_fetch`, `web_search`, `computer_use`,
`notebook_edit`, `task`, `todo`, `workflow_run`. The `workflow_run` tool
(WO 9.1) wraps the `kf-workflow` crate's `WorkflowExecutor` so the
agent loop and bench harness can invoke workflows via tool calls, reusing
the same in-process `TaskSpawner` as the `task` tool. Plugin tools are
registered alongside these at runtime.

The `bash` tool has two isolation layers: Docker execution mode
(`--docker`, ADR-036) for full container isolation, and lightweight
rlimit hardening (`--harden`, ADR-054) for the non-Docker path. The
`--harden` flag applies `RLIMIT_CPU` / `RLIMIT_AS` / `RLIMIT_FSIZE` to
the child shell in a `pre_exec` hook (Unix only; Windows no-op with a
warning). It is ignored when `--docker` is set (Docker already enforces
`--memory` and `--cpus`). seccomp is documented as future work in
ADR-054 — it needs a BPF compiler that's too heavy for the
size-optimized binary.

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
`AppState::completion_suggestions`, rendered as a one-line dim hint above the
input text. The completion layer is `complete_command` (pure, over `COMMANDS`)
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

### `shared/` — cross-cutting types

`Config` (decomposed into 5 `#[serde(flatten)]` sub-structs: `ModelConfig`,
`SecurityConfig`, `ToolConfig`, `SessionConfig`, `DisplayConfig`), `Message`,
`Role`, `StreamEvent`, `ToolDef`, `ToolOutcome`, `ModelInfo`, `ContentPart`,
metrics, backoff, permissions, minify, audit. The audit log records
destructive tool calls (`AuditEntry::Tool`) and hook denials / fail-open
failures (`AuditEntry::Hook`, WO 11.6 / ADR-061) as append-only NDJSON
with a `"kind"` tag.

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
declared by plugins).

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
for backward compatibility. The cross-language NDJSON wire bridge (Rust ↔ TS
orchestrator) shipped in WO 10.8: the `TsOrchestratorBridgeVerifier` in
`bus.rs` shells out to the TS orchestrator's bridge emitter
(`bridge-emitter.ts`), reads NDJSON verdicts from stdout, and translates
each line to a `VerdictEntry`. The wire format is one JSON object per line
(`{"verifier":"security","severity":"error","file":"...","line":N,"message":"...","rule":"..."}`);
malformed lines become `Severity::Warning` verdicts (never silently dropped).
The Rust `VerifierBus` is authoritative: built-in verifiers register
directly, plugin verifiers register via `register_plugin_verifiers_into_bus`,
and TS orchestrator emitters register via the `TsOrchestratorBridgeVerifier`.

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
window. Four modes: `off`, `lite`, `full`, `ultra`. The pipeline applies
content-type-specific transforms with offload storage and query-based relevance
filtering.

Stratum ships as a compiled-in module (when the `stratum` feature is on,
ADR-046) or as a standalone `stratum` binary (feature off, shell fallback).
The `session-start` hook emits the active ruleset so the model knows the
compression contract; the `pre-tool-bash` hook validates config to surface
drift early. Both hooks are in-process Rust handlers when compiled in.

Stratum also coordinates with the Plugin3 budget guard (Workorder 8.6,
ADR-051): when the budget slices a tool result, a registered Stratum listener
compresses the sliced display so the model sees a single coordinated
post-compression size, and the Stratum session mode auto-escalates `Lite →
Full` when the budget is `Approaching`. The coordination is a sync
registered-listener dispatch (not the async `EventBus`) because the slice path
is itself sync.

---

## Token budget (Plugin3)

Plugin3 is the **output-side** context cost system. It tracks token spend
against a configurable ceiling (default 200K) and intervenes when the budget is
approached or exceeded:

| State | Action |
|---|---|
| `Under` | Allow |
| `Approaching` (≥80% of ceiling) | Warn; auto-escalate Stratum `Lite → Full` (Workorder 8.6) |
| `Over` | Slice the largest recent tool output, or compact if no single slice fits |

The orchestrator (`SlicingOrchestrator`) classifies tool outputs, slices
oversized ones with head/tail markers, and offloads the full content to a store.
Cost reporting tracks per-turn usage. Plugin3 ships as a compiled-in module
(when the `budget` feature is on, ADR-047) or as a standalone `kf-budget` binary
(feature off, shell fallback).

The 4 in-process hooks receive full `HookContext` with real tool result content
and compact metadata — the lossy canned-JSON shim that existed when Plugin3 ran
as a shell plugin is eliminated (ADR-047). The hooks observe and report budget
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

The four folded plugins (Stratum, Plugin3, Draw, Video) use this two-path
dispatch. A single toggle — `enabled_plugins` in `ToolConfig` — controls both
paths: a folded plugin name enables the compiled-in path (feature on) or the
shell path (feature off). As of WO 15.7 (item 5.1), the runtime toggle also
gates the compiled-in path: when a folded plugin name is absent from
`enabled_plugins`, its tools and in-process hooks are not registered even
when the compile-time feature is on. So `/plugins disable stratum` removes
"stratum" from `enabled_plugins` and the Stratum tools/hooks stay live only
on the next `kf-code run` that re-registers them. `plugin_sources` is only
needed for external/shell plugins. The `kf-plugin-sdk` self-plugin (Node
SDK) is **not** folded; it stays an external shell-out under all
configurations because its tools depend on the Node ecosystem (ESLint,
TypeScript, Ruff, Pyright, Bandit).

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

### The 5 built-in plugins

| Plugin | Trust | Skills | Tools | Hooks | Source |
|---|---|---|---|---|---|
| `kf-plugin-sdk` | shell | `/kf-code` | 6 | 0 | External — Node SDK (`npm/kf-plugin`), not folded |
| `stratum` | shell | `/stratum` | 5 | 2 | Compiled-in (`stratum` feature) or external (`stratum` binary) |
| `kf-budget` | shell | `/budget` | 7 | 4 | Compiled-in (`budget` feature) or external (`kf-budget` binary) |
| `kf-draw` | shell | `/draw` | 1 | 1 | Compiled-in (`draw` feature) or external (`kfd` binary) |
| `kf-video` | shell | `/video` | 8 | 0 | Compiled-in (`video` feature) or external (`kf-video` binary) |

Runtime toggles: `enabled_plugins` (Vec) and `plugin_sources` (HashMap) in
`ToolConfig`. The `/plugins` TUI command set: `list`, `enable`, `disable`,
`toggle`, `reload`, `trust`, `sources`, `add`, `remove`, `setup`.

---

## Specialized runtimes

### Draw

Draw is a terminal diagram editor (`kfd` binary) with a pure document model
(`kf-draw-core`). The model plans a diagram and emits a `.td.json` file;
the `draw_render` tool renders it to fenced markdown via `kfd --render --fenced`.
A `post-turn` hook suggests rendering any new `.td.json` files. The document
format is pinned in ADR-0003.

Draw's architectural role: a **visual artifact surface** for the agent. The model
produces structured diagram descriptions; `kfd` renders them. It is not a drawing
application for humans — it is an output renderer for agent-produced diagrams.

### Video

Video is an instruction-driven video production pipeline (`kf-video`
binary). The text LLM is the **director**: it writes a brief, selects a pipeline
(`animated_explainer`, `cinematic`, `screen_demo`), plans scenes, and invokes
the video binary to render via FFmpeg. The video model (if configured) generates
assets; the text LLM edits and assembles.

Video's architectural role: a **specialized execution environment** for
agent-driven video editing. The pattern is:

```
User → LLM (director) → timeline operations → asset selection →
video binary (render) → LLM reviews output
```

This is fundamentally different from "generate a video." It treats video editing
as an agent orchestration problem where the text model directs and the video
model executes.

### Workflow engine

`kf-workflow` is a programmable JSON workflow engine. Workflows are DAGs
of persona-driven steps (`explore`, `plan`, `coder`) with optional critique
passes. Three built-in templates ship: `bugfix`, `feature`, `refactor`.
Workflows reuse the `task` tool's in-process spawner, so they run as orchestrated
subagent personas within a single session. Workflows are invoked two ways: the
TUI `/workflow run` slash command, and the `workflow_run` tool (WO 9.1) which
lets the agent loop and bench harness run a named template via a tool call.

---

## Benchmarks

The benchmark system measures agent capability on coding tasks across three
difficulty levels. The task set is organized against the [KIRK-BENCH](../KIRK-BENCH.md)
spec: eight categories (Repository Understanding, Refactoring, Bug Fixes, New
Features, Verification, Context Intelligence, Real Engineering, Cost), 40
numbered tasks, one universal scoring format, 10 hero benchmarks, and one
signature challenge — the **Token Budget Challenge** (WO 14.7, ADR-0066). The
spec documents 40 tasks; 31 are implemented today and mapped to the categories
in `KIRK-BENCH.md`'s mapping table; the remaining ~9 are listed as planned
(honest deferral — each exercises a specific feature and is a future WO).

The 31 implemented tasks break down as follows: 20 are single-file coding-skills tasks
(Rust refactors, bug fixes, doc/test additions), 4 are plugin-tool tasks
(`use_stratum_compress`, `use_budget_check`, `use_draw_render`,
`use_lsp_query`) that exercise the Stratum, Plugin3, Draw, and LSP tool
wrappers respectively, 1 is a workflow-tool task (`use_workflow_run`),
5 are multi-file/multi-turn tasks (`multi_file_pattern`, `test_fix_cycle`,
`pr_review`, `refactor_trait_extraction_multi`, `debug_log_trace`) added in
WO 9.9 to exercise real agent skills (pattern-following, test-fix cycles,
PR review, trait extraction, stack-trace debugging), and 1 is the signature
challenge (`token_budget_challenge`, WO 14.7). Each task is a TOML
file with a prompt, optional setup files, and a deterministic verify spec
(`test_passes`, `file_contains`, or `command_exits_zero`). All tasks use
synthetic `setup_files` so they do not depend on the live repo state.

The 5 multi-file tasks use `requires_model = true` (a `BenchTask` field
added in WO 9.9, default false) because their verify specs check
*post-model* content (cargo build/test, grep for the new symbol the model
was asked to create). `bench verify-only` skips these and reports
`[SKIP] skipped (requires model)`; `bench run` runs them normally. This
fixes the WO 9.0 anti-pattern where verify specs grepped setup content,
passing `verify-only` trivially without validating the model's work.

The harness (`kf-bench` crate + `src/session/bench.rs`) spins up a
headless agent session with a real model adapter, auto-approves all tool calls,
runs the task, then verifies the result deterministically. Reports are written as
JSON and markdown.

### Token Budget Challenge (WO 14.7, ADR-0066)

The signature benchmark. It runs the same task 5× under descending context
budgets (128k → 64k → 32k → 16k → 8k) and records six metrics per ceiling:
success, prompt tokens, completion tokens, compression passes, cost. This
showcases the tree-sitter context index, Stratum compression, and the Plugin3
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

A `bench` CI job runs all tasks on Ollama with `qwen2.5:0.5b` on every push/PR.
It posts a delta summary as a PR comment comparing against the `main` baseline
(ADR-045). A nightly `bench-baseline` workflow on `main` stores the
canonical baseline report as a workflow artifact.

### Bench CI loop (WO 10.9)

The bench CI loop has three jobs in `.github/workflows/bench-baseline.yml`:

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

### Coverage gate (WO 12.9, ADR-065)

The `coverage` CI job runs `cargo tarpaulin --out Xml --locked --lib
--timeout 120 -- --skip test_build_fork_tree_nests_children`. `--lib`
instruments unit tests only (integration tests in `tests/` spawn
ollama/cargo subprocesses and are excluded from coverage), and the
`--skip` is belt-and-suspenders against the WO 12.0 tarpaulin flake
(root cause fixed; the skip stays pending a verified flake-free run).

A Python gate parses the Cobertura XML and fails the job when any of the
three gated `src/` prefixes drops below its threshold: `src/session`
(68.5), `src/tools` (76.0), `src/adapters` (75.0). The thresholds are
set at or just below the *measured* coverage (the headroom policy,
ADR-065): zero headroom at first catches regressions immediately,
relaxing by at most 2 points only if a one-line flake fails CI
repeatedly. The 12-series target was 75% on all three. `src/tools`
(≈76.5%) and `src/adapters` (≈84%) clear it and their floors are at/above
75; `src/session` (~68.6%) is honestly deferred to a follow-up
workorder — the remaining gap is async executor + MCP-HTTP code that
needs integration test work, not pure-helper unit tests. The gate is a
regression guard, not a vanity number.

---

## Feature flags

The root `Cargo.toml` exposes these features:

- `stratum` (default) — folds the Stratum context-compression plugin in as
  direct Rust calls (ADR-046).
- `draw` (default) — folds the Draw diagram plugin in as direct Rust calls
  (ADR-048).
- `budget` (default) — folds the Plugin3 token-budget guard in as direct
  Rust calls with full in-process event context (ADR-047).
- `video` (non-default) — folds the Video plugin in as direct Rust calls.
  Off by default because it pulls `serde_yaml`, `strum`, and `which` (new
  transitive deps); users who want agent-driven video editing opt in via
  `--features video` (ADR-049).
- `otel` (non-default) — OpenTelemetry export.

Four plugins are therefore feature-gated compiled-in modules, served as
direct Rust calls when their feature is on and falling back to the shell
plugin path when it is off (graceful degradation). ADR-050 pins the
two-path dispatch consolidation design. The `dep:` optional-dependency
pattern is what makes per-plugin opt-in possible.

ADR-0017's "no `[features]` section" rule is scoped to `crates/kf-budget-core/`,
not the root binary.

---

## ADRs

84 Architecture Decision Records live in [docs/adr/](docs/adr/). They pin
load-bearing decisions: token budget (0005), slicing orchestrator (0007),
verifier bus (0028, 0043), context index (037), benchmark harness (038),
execution replay (039), VFS minification (053), coverage-gate threshold
policy (065), and many more. A drift test (`adr_xref_drift`) enforces that
ADR file headers and the README index table agree.

Conventions: `ponytail:` annotations pin spec literals (if a ponytail test
fails, the spec and impl drifted, not the test). `ceiling:` and `upgrade path:`
document known limitations. Removing these is a regression.

---

## Where to go next

- **README.md** — user-facing landing page
- **[docs/adr/](adr/)** — pinned decisions and their rationale
- **[docs/workorders/](workorders/)** — planned and in-progress work
- **[AGENTS.md](../AGENTS.md)** — worker contract for AI agents in this repo
- **[state.md](../state.md)** — current production-readiness state
- **[CHANGELOG.md](../CHANGELOG.md)** — release history