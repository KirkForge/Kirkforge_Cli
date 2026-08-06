# Skills System

**Source:** vix (`internal/agent/skills.go`, `internal/config/defaults/agents/`)
**Goal:** User-definable slash commands via `SKILL.md` files. Declarative YAML frontmatter for model, tools, description. No recompilation needed.

## Why

- Hardcoded slash commands (`/help`, `/clear`, `/model` in `src/tui/mod.rs`) are limited and require recompilation
- Users should define their own: `/commit`, `/review`, `/test`, `/deploy`
- Each skill controls its own tool allowlist (e.g., `/review` only gets read-only tools)
- Skills can target different models per action (cheap model for lint, expensive for architecture)

## SKILL.md Format

```markdown
---
name: commit
description: Generate a conventional commit message from staged changes
model: qwen2.5:0.5b     # optional — override default model
tools: [bash, read_file] # optional — restrict tool availability
max_turns: 3             # optional — cap iterations
---
Generate a concise git commit message from the current staged changes.
Use `git diff --cached` to inspect changes.
Follow conventional commits format: `type(scope): description`
Keep the subject under 72 characters.
```

Resolution order:
1. `./.kirkforge/skills/<name>/SKILL.md` (project-level, highest priority)
2. `~/.config/kirkforge/skills/<name>/SKILL.md` (user-level)
3. Built-in skills (compiled in)

## Architecture Sketch

```rust
pub struct Skill {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub max_turns: Option<usize>,
    pub prompt: String,
}

pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn load() -> Self {
        let mut skills = HashMap::new();
        // 1. Built-in skills
        skills.insert("help", Skill::builtin_help());
        skills.insert("clear", Skill::builtin_clear());
        // 2. User-level (~/.config/kirkforge/skills/*/SKILL.md)
        // 3. Project-level (.kirkforge/skills/*/SKILL.md)
        Self { skills }
    }

    pub fn execute(&self, name: &str, input: &str, executor: &mut Executor) -> Result<Vec<TurnEvent>> {
        let skill = self.skills.get(name)?;
        // Override executor config with skill's model/tool settings
        // Run the turn loop with the skill's prompt + user input
    }
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/tui/mod.rs` | Replace hardcoded `/help`/`/clear`/`/model` with `SkillRegistry::dispatch()` |
| `src/session/` | New `skills.rs` module — `Skill`, `SkillRegistry` |
| `src/session/executor.rs` | Accept optional skill override (model, tool filter, max_turns) |

## Built-in Skills

| Skill | Description |
|-------|-------------|
| `/help` | List all available skills |
| `/clear` | Clear conversation |
| `/model` | Show/set current model |
| `/review` | Code review of current changes (read-only tools) |
| `/commit` | Generate commit message |
| `/test` | Run tests and report results |
| `/explain` | Explain the selected code |

## Comparison: Skills vs Event Bus

Skills are LLM-driven (user asks, LLM responds, constrained tools). The event bus is deterministic (pre-fire checks, no LLM involved). They complement each other: the event bus feeds context into every turn, skills provide structured prompts for common workflows.