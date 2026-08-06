# Path Safety — Atomic Writes & Guards

**Source:** KirkForge-Plugin (`packages/orchestrator/src/path-safety.ts`)
**Goal:** Every file write goes through a 10-point safety guard system before touching disk. Prevents the LLM from writing outside the workspace, overwriting critical files, or creating dangerous binaries.

## The 10 Guards

From `path-safety.ts`, in evaluation order:

1. **Sandbox containment** — resolved path must start with `cwd`
2. **Extension policy** — only allowed extensions (`.rs`, `.py`, `.js`, `.ts`, `.md`, etc.)
3. **Hidden dotfile block** — `.env`, `.npmrc`, `.ssh`, `.gitconfig`, `.aws/` — blocked
4. **Hidden directory block** — no `.git/`, `.vscode/`, `.idea/` — exceptions for `.vscode` and `.idea`
5. **Symlink traversal** — every path segment checked; refuse to write through symlinks
6. **Final symlink guard** — if the target is itself a symlink, refuse
7. **Size limit** — max 1 MB per artifact
8. **Binary detection** — >30% non-printable chars in first 8KB → refuse
9. **Overwrite policy** — deny by default unless `allow_overwrite` is set in config
10. **Deny list check** — matches against configured deny paths

## Atomic Write Pattern

```rust
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<(), IoError> {
    let tmp = path.with_extension(format!(".kirkforge_tmp_{}", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(content)?;
    f.sync_all()?;           // fsync for crash safety
    tmp.rename(path)?;       // atomic on same filesystem
    Ok(())
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/tools/write_file.rs` | Add 10-point guard pipeline before write, switch to atomic write |
| `src/tools/edit_file.rs` | Same guard pipeline |
| `src/shared/mod.rs` | Add `PathGuard` struct with `check_all()` method |
| `src/session/config.rs` | Add path policy to config (allowed_extensions, deny_paths, max_file_size) |