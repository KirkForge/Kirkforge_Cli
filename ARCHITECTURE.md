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
| Context quality | Tree-sitter symbol/import/call-graph index (`kirkforge-context-index`) gives the agent graph-grounded retrieval instead of plain-text search. Four languages: Rust, TypeScript, Python, Go. |
| Context cost (input side) | Stratum compression pipeline classifies and compacts bloated tool outputs *before* they enter the context window. |
| Context cost (output side) | Plugin3 budget guard tracks token spend against a ceiling and slices or compacts oversized tool results when the budget is approached. |
| Execution reliability | A verifier bus runs build, test, lint, rustfmt, git-state, and security checks after file-modifying tool calls. A correction loop auto-applies formatter fixes and feeds unfixable errors back to the model as tool results. |
| Reproducibility | Enforced plan mode (`/plan` then `/implement`), per-result checkpointing mid-batch, execution replay (ADR-039), and conversation logging. |
| Extensibility | A manifest-based plugin system (`kirkforge.toml`) with trust tiers, minisign signature verification, and four capability kinds: skills, tools, hooks, verifiers. |

---

## Workspace layout

The workspace has one binary crate (`kirkforge`) and 16 satellite crates under
`crates/`. The binary is the user-facing CLI; the satellites are libraries and
standalone binaries.

```
kirkforge (root bin)          ← the CLI the user runs
├── src/                       ← agent core (session, tools, TUI, adapters, verifiers)
├── crates/                    ← 16 satellite crates
│   ├── kirkforge-plugin       ← plugin SDK: manifest types, trust tiers
│   ├── kirkforge-plugin-host  ← plugin runtime: registry, dispatch, signatures
│   ├── kirkforge-context-index← tree-sitter symbol/import/call-graph index
│   ├── kirkforge-workflow     ← programmable JSON workflow engine
│   ├── kirkforge-lsp          ← LSP client pool for symbol-aware navigation
│   ├── kirkforge-bench        ← task-benchmark harness (types + verifier + reports)
│   ├── kirkforge-draw-core    ← pure document model for KirkForge-Draw
│   ├── kirkforge-draw         ← kfd: terminal diagram editor binary
│   ├── kirkforge-video        ← instruction-driven video production binary
│   ├── kirkstratum-core       ← context-compression pipeline library
│   ├── kirkstratum-hosts      ← host-specific compression rules
│   ├── kirkstratum-cli        ← stratum: compression CLI binary
│   ├── plugin3-core           ← budget/orchestrator/slicing data model
│   ├── plugin3-hosts          ← host-side budget adapters
│   ├── plugin3-cli            ← plugin3: budget CLI binary
│   └── kirkforge-testdoctor   ← test-performance profiler (excluded from workspace)
├── plugins/                   ← 5 plugin manifests + shell tool/hook scripts
│   ├── kirkforge-plugin/      ← SDK self-plugin (Node-backed verification tools)
│   ├── stratum/               ← compression plugin (5 tools, 2 hooks)
│   ├── kirkforge-plugin3/     ← budget plugin (7 tools, 4 hooks)
│   ├── kirkforge-draw/        ← diagram plugin (1 tool, 1 hook)
│   └── kirkforge-video/       ← video plugin (8 tools)
├── benches/tasks/             ← 10 benchmark task definitions (TOML)
└── docs/adr/                  ← 62 Architecture Decision Records
```

### Compiled-in vs satellite

The root `kirkforge` binary directly depends on six crates:

| Crate | Role |
|---|---|
| `kirkforge-plugin` | Plugin manifest types and trust-tier logic |
| `kirkforge-plugin-host` | Plugin registry, dispatch, signature verification |
| `kirkforge-context-index` | Tree-sitter indexing and graph retrieval |
| `kirkforge-workflow` | JSON workflow engine (reuses the `task` tool's spawner) |
| `kirkforge-lsp` | LSP client pool |
| `kirkforge-bench` | Benchmark task types, loader, verifier, report writers |

The remaining nine crates are **satellites**: they build as standalone binaries
(`kfd`, `kirkforge-video`, `stratum`, `plugin3`) or support libraries. The plugin
system invokes them via shell scripts. Folding them into the core as
feature-gated compiled-in modules is the planned work in Workorder 7.0.

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
- **Plugin tools** (`plugin_tools/`): loads plugin manifests and wraps plugin
  tool commands in `PluginToolWrapper` (implements the `Tool` trait, spawns the
  shell script as a subprocess with a curated env and timeout).
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

### `tools/` — built-in tools

18 tools implementing the `Tool` trait: `read_file`, `write_file`, `edit_file`,
`atomic_write`, `bash`, `bash_cancel`, `bash_minify`, `bash_status`, `glob`,
`grep`, `lsp_query`, `read_image`, `web_fetch`, `web_search`, `computer_use`,
`notebook_edit`, `task`, `todo`. Plugin tools are registered alongside these at
runtime.

### `tui/` — interactive UI

A ratatui-based terminal UI with chat, input, status, search, slash commands,
plugin management, persona switching, session forking/resume, and approval
gates. Drains three event sources (user input, model stream, approval queue) in
a single loop.

### `shared/` — cross-cutting types

`Config` (decomposed into 5 `#[serde(flatten)]` sub-structs: `ModelConfig`,
`SecurityConfig`, `ToolConfig`, `SessionConfig`, `DisplayConfig`), `Message`,
`Role`, `StreamEvent`, `ToolDef`, `ToolOutcome`, `ModelInfo`, `ContentPart`,
metrics, backoff, permissions, minify, audit.

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

`kirkforge-context-index` builds a tree-sitter-backed symbol, import, and
call-graph index. For a given symbol, the agent can retrieve:

- The symbol's definition (file, line, kind)
- Files that import it (`imported_by`)
- Call sites that invoke it (`called_by`)

Four languages: Rust, TypeScript (including tsx), Python, Go. The index is
cached as JSON at `.kirkforge/context-index/cache.json`, keyed on git HEAD for
invalidation. This gives the agent graph-grounded context instead of relying on
plain-text search.

---

## Context compression (Stratum)

Stratum is the **input-side** context cost system. It classifies tool outputs
by content type and compacts bloated payloads *before* they enter the context
window. Four modes: `off`, `lite`, `full`, `ultra`. The pipeline applies
content-type-specific transforms with offload storage and query-based relevance
filtering.

Stratum ships as a standalone `stratum` binary, invoked by the
`plugins/stratum/` plugin (5 tools, 2 hooks). The `session-start` hook emits the
active ruleset so the model knows the compression contract; the `pre-tool-bash`
hook validates config to surface drift early.

---

## Token budget (Plugin3)

Plugin3 is the **output-side** context cost system. It tracks token spend
against a configurable ceiling (default 200K) and intervenes when the budget is
approached or exceeded:

| State | Action |
|---|---|
| `Under` | Allow |
| `Approaching` (≥80% of ceiling) | Warn |
| `Over` | Slice the largest recent tool output, or compact if no single slice fits |

The orchestrator (`SlicingOrchestrator`) classifies tool outputs, slices
oversized ones with head/tail markers, and offloads the full content to a store.
Cost reporting tracks per-turn usage. Plugin3 ships as a standalone `plugin3`
binary, invoked by the `plugins/kirkforge-plugin3/` plugin (7 tools, 4 hooks).

**Known limitation**: under KirkForge, plugin3's hooks emit canned JSON because
the host passes only env vars, not full event context. Folding plugin3 into core
(Workorder 7.0) eliminates this lossy shim.

---

## Plugin system

Plugins are manifest-based and dynamically loaded at runtime from the
filesystem. The plugin SDK (`kirkforge-plugin`) and host (`kirkforge-plugin-host`)
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
shell path (feature off). `plugin_sources` is only needed for external/shell
plugins. The `kirkforge-plugin` self-plugin (Node SDK) is **not** folded; it
stays an external shell-out under all configurations because its tools depend
on the Node ecosystem (ESLint, TypeScript, Ruff, Pyright, Bandit).

`/plugins list` shows the source (`compiled-in` / `external` /
`external (feature off)`) and feature gate for each workspace plugin source.

### Manifest format (`kirkforge.toml`)

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

### Trust tiers

`read-only` < `shell` < `network` < `unsafe`. The host caps plugins at
`max_plugin_trust` (config: default `shell`). Over-tier plugins are rejected or
downgraded. Optional minisign detached-signature verification (`.kirkforge.sig`).

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
| `kirkforge-plugin` | shell | `/kirkforge` | 6 | 0 | External — Node SDK (`npm/kirkforge-plugin`), not folded |
| `stratum` | shell | `/stratum` | 5 | 2 | Compiled-in (`stratum` feature) or external (`stratum` binary) |
| `kirkforge-plugin3` | shell | `/budget` | 7 | 4 | Compiled-in (`budget` feature) or external (`plugin3` binary) |
| `kirkforge-draw` | shell | `/draw` | 1 | 1 | Compiled-in (`draw` feature) or external (`kfd` binary) |
| `kirkforge-video` | shell | `/video` | 8 | 0 | Compiled-in (`video` feature) or external (`kirkforge-video` binary) |

Runtime toggles: `enabled_plugins` (Vec) and `plugin_sources` (HashMap) in
`ToolConfig`. The `/plugins` TUI command set: `list`, `enable`, `disable`,
`toggle`, `reload`, `trust`, `sources`, `add`, `remove`, `setup`.

---

## Specialized runtimes

### Draw

Draw is a terminal diagram editor (`kfd` binary) with a pure document model
(`kirkforge-draw-core`). The model plans a diagram and emits a `.td.json` file;
the `draw_render` tool renders it to fenced markdown via `kfd --render --fenced`.
A `post-turn` hook suggests rendering any new `.td.json` files. The document
format is pinned in ADR-0003.

Draw's architectural role: a **visual artifact surface** for the agent. The model
produces structured diagram descriptions; `kfd` renders them. It is not a drawing
application for humans — it is an output renderer for agent-produced diagrams.

### Video

Video is an instruction-driven video production pipeline (`kirkforge-video`
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

`kirkforge-workflow` is a programmable JSON workflow engine. Workflows are DAGs
of persona-driven steps (`explore`, `plan`, `coder`) with optional critique
passes. Three built-in templates ship: `bugfix`, `feature`, `refactor`.
Workflows reuse the `task` tool's in-process spawner, so they run as orchestrated
subagent personas within a single session.

---

## Benchmarks

The benchmark system measures agent capability on 10 coding tasks across three
difficulty levels (3 easy, 5 medium, 2 hard). Each task is a TOML file with a
prompt, optional setup files, and a deterministic verify spec
(`test_passes`, `file_contains`, or `command_exits_zero`).

The harness (`kirkforge-bench` crate + `src/session/bench.rs`) spins up a
headless agent session with a real model adapter, auto-approves all tool calls,
runs the task, then verifies the result deterministically. Reports are written as
JSON and markdown.

A `bench` CI job runs all tasks on Ollama with `qwen2.5:0.5b` on every push/PR.
It is currently informational (`|| true`) — it does not gate merges. Wiring it
into a continuous evaluation pipeline with baseline comparison and delta
reporting is planned work (Workorder 6.5).

---

## Feature flags

The root `Cargo.toml` has one feature: `otel` (OpenTelemetry export, off by
default). No plugin is currently feature-gated. The `dep:` optional-dependency
pattern is established and will be used to gate plugin fold-in (Workorder 7.0).

ADR-0017's "no `[features]` section" rule is scoped to `crates/plugin3-core/`,
not the root binary.

---

## ADRs

62 Architecture Decision Records live in [docs/adr/](docs/adr/). They pin
load-bearing decisions: token budget (0005), slicing orchestrator (0007),
verifier bus (0028, 0043), context index (037), benchmark harness (038),
execution replay (039), and many more. A drift test (`adr_xref_drift`) enforces
that ADR file headers and the README index table agree.

Conventions: `ponytail:` annotations pin spec literals (if a ponytail test
fails, the spec and impl drifted, not the test). `ceiling:` and `upgrade path:`
document known limitations. Removing these is a regression.

---

## Where to go next

- **README.md** — user-facing quick start and feature list
- **[docs/adr/](docs/adr/)** — pinned decisions and their rationale
- **[docs/workorders/](docs/workorders/)** — planned and in-progress work
- **[AGENTS.md](AGENTS.md)** — worker contract for AI agents in this repo
- **[state.md](state.md)** — current production-readiness state
- **[CHANGELOG.md](CHANGELOG.md)** — release history