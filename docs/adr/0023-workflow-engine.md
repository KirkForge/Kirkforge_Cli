# ADR-023: Programmable JSON Workflow Engine

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

Vix's biggest daily-driver advantage over KirkForge is its multi-step
workflow engine (`internal/daemon/workflow.go` + `workflow_state.go` +
`plan_workflow/{explore,plan,critique,execute,refine}.md`). For "build
this feature end-to-end" work, users want a repeatable, auditable
pipeline rather than a single subagent prompt.

This ADR scopes a workflow engine that fits v0.2: user-editable JSON
files, dependency-order execution, persona-driven tool restrictions, and
reuse of the existing `task` tool infrastructure.

## Decision

Introduce a vendored workspace member `crates/kirkforge-workflow/` that
owns the schema, validation, dependency resolution, and executor. The
main crate wires it into the TUI as `/workflow run`, `/workflow
status`, and `/workflow cancel`.

### Schema

A workflow is a named list of steps:

```json
{
  "name": "add-feature",
  "steps": [
    {"name": "explore", "prompt": "...", "persona": "explore"},
    {"name": "plan", "prompt": "...", "persona": "plan", "depends_on": ["explore"]},
    {"name": "critique", "prompt": "...", "persona": "plan", "depends_on": ["plan"]},
    {"name": "execute", "prompt": "...", "persona": "coder", "depends_on": ["critique"]},
    {"name": "refine", "prompt": "...", "persona": "coder", "depends_on": ["execute"]}
  ]
}
```

- `name` — unique identifier within the workflow.
- `prompt` — sent to the subagent, with dependency summaries appended.
- `persona` — `explore`, `plan`, or `coder`; maps to the existing
  persona tool restrictions in the `task` tool.
- `depends_on` — list of prior step names. The executor only schedules a
  step after all its dependencies complete.
- `critique` — optional bool. When true, the step is also run through
  the `plan` persona as a critique and the critique output is appended
  to the step summary.

Validation rejects duplicate names, unknown personas, unknown or
self-dependencies, and dependency cycles.

### Executor

`WorkflowExecutor::run` iteratively computes the ready frontier,
generates per-step prompts that include dependency summaries, and
invokes a `StepRunner` trait for each step. The binary crate implements
`StepRunner` with the existing `InProcessTaskSpawner` so every workflow
step is literally a `task` tool call with extra context.

The batch of ready steps is run sequentially in the current
implementation. Parallelism is left to the `StepRunner` implementation;
WO-2 (parallel `task` dispatch) can fan out independent steps when it
lands without changing the schema or executor.

### Loading

Workflow files are JSON loaded from:

1. `.kirkforge/workflows/<name>.json` in the current directory.
2. `~/.local/share/kirkforge/workflows/<name>.json`.

Built-in templates (`feature.json`, `bugfix.json`, `refactor.json`) are
shipped under `crates/kirkforge-workflow/templates/` as defaults. They
are copied to the user share directory on first use; users may edit or
override them because the JSON format is open, not a closed system.

### TUI commands

- `/workflow run <name>` — load and start a workflow.
- `/workflow status` — show step progress.
- `/workflow cancel` — abort the running workflow.

Rendering is a simple step list for v0.2. Terminal diagram editor
integration is explicitly deferred.

### Out of scope

- **Whiteboard mode** — Vix's infinite-canvas planning UI is a Vix
  concept; KirkForge has `/draw` for a different purpose. Not
  implemented in v0.2.
- **Parallel subagent orchestrator** — a workflow step is just a `task`
  tool call. We reuse `InProcessTaskSpawner` and the persona-specific
  toolset.
- **YAML / TOML** — JSON keeps parsing simple, is trivially edited by
  users, and matches the existing tool schemas in the project.

## Consequences

- Users can codify feature, bugfix, and refactor pipelines without
  changing Rust code.
- The `task` tool and persona restrictions get reuse; no duplicate
  subagent runtime is built.
- Dependency propagation gives each step the right context automatically.
- Cycle and validation errors are caught before any model call runs.
- Sequential scheduling works today; WO-2 can add real parallel fan-out
  later without a breaking change.

## Alternatives considered

1. **Vix's `workflow.go` model directly** — Vix couples workflows to
   session forking and prompt-cache stem sharing. KirkForge already has
   `/explore`, `/plan`, `/coder` personas via the `task` tool, so the
   DAG + context model is the right subset to port.
2. **YAML instead of JSON** — YAML is more human-editable for multi-line
   prompts, but it adds a parser dependency and ambiguity around
   strings. JSON is already used everywhere for tool schemas and is
   sufficient for v0.2.
3. **Whiteboard mode** — Rejected; it is a separate UI paradigm and not
   needed for the CLI daily-driver gap.

## ponytail

- Executor currently runs ready steps sequentially. Once WO-2 lands,
  swap `for` over the ready batch for `FuturesUnordered` or
  `join_all`, keyed by step name, to run independent `explore` steps in
  parallel.
- `WorkflowHandle` in `AppState` can be extended later to expose
  per-step timing / cost if cost tracking is added to `StepRunner`.

## ceiling

- Large workflows with many independent steps will be slower than Vix
  until WO-2 parallel dispatch is available.
- JSON prompts are one-line escaped strings; very long prompts are
  harder to edit than TOML frontmatter. We can add a CLI helper to
  pretty-print templates in the future.

## upgrade path

- When WO-2 lands, change only the `StepRunner` implementation to fan
  out independent steps; the `WorkflowExecutor` batch contract stays
  the same.
- Future work can add template variables (`{{user_prompt}}`, `{{step.X}}`)
  without schema breakage because extra fields are ignored by
  `serde_json`.
