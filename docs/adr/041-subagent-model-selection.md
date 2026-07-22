# ADR-041: Subagent model selection

## Status

Accepted

## Context

The tiered brain+brawn workflow requires subagents to run different models than the parent. OpenCode's agent system supports per-agent model override. KirkForge's `TaskRequest` had no `model` field — every subagent inherited the parent's model. This made it impossible to use a cheap model (e.g. `opencode/big-pickle`) for exploration subtasks while keeping a powerful model for the parent.

## Decision

1. Add `model: Option<String>` to `TaskRequest`. When `Some(model_name)`, the subagent uses that model; when `None`, it inherits the parent's model.
2. Add `subagent_allowed_models: Option<Vec<String>>` to `Config`. When set, subagent model choices are restricted to the allowlist. Any model not on the list is rejected and the subagent falls back to the parent model.
3. The effective model is computed as `effective_model = request.model.filter(|m| allowed.contains(m)).unwrap_or(parent_model)`.

## Consequences

- **Positive:** Cost optimization via tiered models (brain spawns brawn on a free/cheap model). Subagent model is independently configurable per task.
- **Negative:** Subagent model must be available on a reachable provider. Misconfigured allowlist can silently downgrade subagents to the parent model. The allowlist is a flat list — no wildcards or tier-based shortcuts yet.