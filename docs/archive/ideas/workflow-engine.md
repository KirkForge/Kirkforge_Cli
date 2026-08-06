# Workflow Engine

**Source:** vix (`internal/daemon/workflow.go`), claude-code-rust (`src/plugins/`)
**Goal:** Define multi-step coding sessions as DAG workflows (Explore → Plan → Refine → Execute → Review) with step forking, variable interpolation, and structured output.

## Why

Single-turn chat works for simple tasks. Real coding needs:
1. **Explore** the codebase first
2. **Plan** the approach
3. **Refine** based on critique
4. **Execute** the changes
5. **Review** what changed

A workflow engine makes this repeatable, auditable, and cache-friendly (fork_from shares conversation history across phases).

## Workflow Format (TOML)

```toml
[[workflows]]
name = "plan-execute"
entry_point = "explore"

[workflows.steps.explore]
type = "agent"
prompt = "Explore the codebase to understand {{task}}"
tools = ["read_file", "grep", "glob", "bash"]
silent = true

[workflows.steps.plan]
type = "agent"
prompt = """
Based on exploration, create a plan:
$(explore.output)
"""
fork_from = "explore"  # share conversation history = cache hit
tools = ["read_file"]

[workflows.steps.refine]
type = "bash"
command = "cat plan.md | head -20"
fork_from = "plan"

[workflows.steps.execute]
type = "agent"
prompt = "Implement the plan: $(plan.output)"
fork_from = "refine"
tools = ["read_file", "write_file", "edit_file", "bash", "grep"]

[workflows.steps.review]
type = "agent"
prompt = "Review the changes from step 4"
fork_from = "execute"
tools = ["read_file", "grep", "bash"]
```

## Step Types

| Type | Behavior |
|------|----------|
| `agent` | Full LLM turn with tool access |
| `bash` | Run a command, capture output |
| `tool` | Prompt user for input (structured question) |

## Variable Interpolation

| Syntax | Resolves to |
|--------|-------------|
| `$(step.output)` | The output text of step `step` |
| `$(workflow.prompt)` | The original user prompt that started the workflow |
| `$(session.id)` | The current session ID |
| `$(file:path)` | Contents of file at `path` |

## Integration Points

| File | Change |
|------|--------|
| `src/` | New `workflow/` module — `engine.rs`, `step.rs`, `template.rs` |
| `src/main.rs` | Add `--workflow` CLI flag |
| `src/session/executor.rs` | Workflow step runs an executor turn internally |
| `src/tui/app.rs` | Workflow progress display step-by-step |
| `src/shared/mod.rs` | New `WorkflowDef`, `StepDef`, `WorkflowState` types |

## Caching Strategy (fork_from)

Each step's `fork_from` creates a new executor that shares prior conversation history. The system prompt is identical across all steps (the "stem agent" pattern), so prompt cache hits across phases. This is the key optimization — vix's README specifically calls this out as the primary cost-saving pattern.