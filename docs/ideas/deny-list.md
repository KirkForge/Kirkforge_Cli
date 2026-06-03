# Deny List & Access Control

**Sources:** vix (`internal/config/defaults/settings.json` deny_list, `internal/daemon/tool_validation.go`), KirkForge-Plugin (`packages/orchestrator/src/path-safety.ts`)
**Goal:** Safety gates that prevent the LLM from reading/writing outside approved paths, following symlinks into unexpected locations, or executing dangerous commands.

## Why

- LLMs hallucinate file paths
- LLMs follow symlinks out of the project directory
- A bad bash command can wipe the system
- Users need control over what the agent can access without remembering to approve every call

## Features to Port

### 1. Path Deny List

```rust
pub struct DenyList {
    pub paths: Vec<String>,    // glob patterns, e.g. ".env", ".ssh/*", "vendor/"
    pub urls: Vec<String>,     // domain suffixes, e.g. "internal.company.com"
}
```

- Takes precedence over all approvals
- Evaluated for every read/write/bash operation
- Paths are resolved against real filesystem (symlink-safe) before checking
- Users add to deny list via config file

### 2. Symlink Traversal Guards

Every path resolution checks every path segment:
```rust
fn resolve_safe(base: &Path, target: &Path) -> Result<PathBuf, AccessError> {
    let resolved = base.join(target).canonicalize()?;
    // Check every component for symlinks
    for ancestor in resolved.ancestors() {
        if ancestor.is_symlink() && !ALLOWED_SYMLINKS.contains(ancestor) {
            return Err(AccessError::SymlinkTraversal);
        }
    }
    // Must resolve inside base
    if !resolved.starts_with(base) {
        return Err(AccessError::EscapedBase);
    }
    Ok(resolved)
}
```

### 3. Read-Before-Edit Gate

Track files read during the session. If the model tries to edit a file it hasn't read, block it with a warning:

```rust
pub struct ReadGate {
    read_files: HashSet<PathBuf>,
}

impl ReadGate {
    pub fn record_read(&mut self, path: &Path) { self.read_files.insert(path.to_owned()); }
    pub fn check_write(&self, path: &Path) -> Result<(), String> {
        if !self.read_files.contains(path) {
            Err("File was never read before edit — possible hallucination".into())
        } else {
            Ok(())
        }
    }
}
```

### 4. Tool Reason Fields

Add a required `reason` parameter to all destructive tools. The LLM must explain *why* it's calling the tool:

```rust
// In write_file, edit_file, bash tool definitions:
"reason": {
    "type": "string",
    "description": "Why are you calling this tool? Explain the specific goal."
}
```

In dev/debug builds, add a second field: `reason_to_use_instead_of_X` — the LLM must explain why it chose this approach over alternatives. This surfaces hallucinations early.

### 5. Automatic Directory Access

When the LLM tries to read outside the current approved scope, automatically grant access to the new directory rather than requiring per-file approval:

```rust
pub fn auto_grant_access(path: &Path, allowed_dirs: &mut HashSet<PathBuf>) {
    let parent = path.parent().unwrap_or(path);
    allowed_dirs.insert(parent.to_owned());
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/tools/bash.rs` | Add `reason` param, add path resolution with deny check |
| `src/tools/write_file.rs` | Add symlink guard, read-before-edit check |
| `src/tools/edit_file.rs` | Same guards as write_file |
| `src/tools/read_file.rs` | Add deny check, track in ReadGate |
| `src/session/` | New `access.rs` module — `DenyList`, `ReadGate`, `resolve_safe()` |
| `src/shared/mod.rs` | Add `DenyList` to `Config` |
| `src/session/config.rs` | Load deny list from config file |

## Config Format

```toml
[access]
deny_paths = [".env", ".ssh/*", "node_modules/", "vendor/"]
deny_urls = ["internal.corp.com"]

[access.auto_approve]
directories = ["/home/user/projects/*"]
```