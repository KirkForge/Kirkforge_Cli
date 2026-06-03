# Config Bootstrap & Layered Config

**Source:** vix (`internal/config/paths.go`, `internal/config/bootstrap.go`, `internal/config/defaults/`)
**Goal:** First-run auto-generates default config + agent definitions. Config is resolved from layered directories: project-level overrides user-level, which overrides built-in defaults.

## Resolution Order

```
1. Built-in defaults       (compiled into binary via include_str!)
2. ~/.config/kirkforge/     (user-level)
3. ./.kirkforge/            (project-level, highest)
```

Each layer can override specific keys. Settings are shallow-merged: project config overrides user config for matching keys, user config fills in what built-in doesn't set.

## First-Run Bootstrap

On first run (no `~/.config/kirkforge/` exists):

```rust
pub fn bootstrap() -> Result<()> {
    let dir = config_dir()?;
    if dir.exists() {
        return Ok(());  // already bootstrapped
    }

    // Create directory structure
    fs::create_dir_all(dir.join("skills"))?;
    fs::create_dir_all(dir.join("sessions"))?;
    fs::create_dir_all(dir.join("logs"))?;

    // Write default config
    fs::write(dir.join("config.toml"), include_str!("../defaults/config.toml"))?;

    // Write built-in skills
    for skill in BUILTIN_SKILLS {
        let skill_dir = dir.join("skills").join(skill.name);
        fs::create_dir_all(&skill_dir)?;
        fs::write(skill_dir.join("SKILL.md"), skill.content)?;
    }

    tracing::info!("Bootstrapped config at {}", dir.display());
    Ok(())
}
```

## Env Var Overrides

Environment variables take precedence over all config layers:

| Env Var | Overrides |
|---------|-----------|
| `KIRKFORGE_HOST` | `config.ollama_host` |
| `KIRKFORGE_MODEL` | `config.default_model` |
| `KIRKFORGE_MAX_TOKENS` | Token budget override |
| `KIRKFORGE_ALLOWED_DIRS` | Access auto-approve directories |

## CLI Flags Win All

CLI flags (`--model`, `--host`, `--auto-approve`) override everything. These are persisted to config after each run (already implemented).

## Integration Points

| File | Change |
|------|--------|
| `src/session/config.rs` | Add bootstrap(), layered path resolution, env var fallback |
| `src/session/mod.rs` | Call `bootstrap()` from `load_or_create_config()` |
| `src/defaults/` | New directory: `config.toml`, built-in skill files |
| `src/main.rs` | CLI flags override env vars, which override config, which override defaults |