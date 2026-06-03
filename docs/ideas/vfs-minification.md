# VFS & Tree-Sitter Minification

**Source:** vix (`internal/daemon/vfs.go`, `internal/config/defaults/settings.json` languages section)
**Goal:** Upgrade from regex-based minifier to tree-sitter AST-aware minification with per-language formatters. The VFS stores minified versions; writes go through a formatter to restore valid source.

## Current State

`src/shared/minify.rs` — hand-written comment strippers for `.rs`, `.py`, `.js`, `.ts`, `.go`, `.md`. Works but:
- Fragile: misses edge cases (JSX, docstrings, heredocs)
- No restore path: minified output is irreversible
- No per-language configuration (keep_comments, max_line_length)

## Target State

```rust
pub enum MinifyMode {
    StripComments,
    CollapseWhitespace,
    ShortenIdentifiers,  // not safe in all languages
}

pub struct VfsConfig {
    pub enabled: bool,
    pub keep_comments: bool,
    pub formatter: Option<String>,   // external command, e.g. "rustfmt"
}

// Per-language VFS config (from config.toml)
"rust" => VfsConfig { enabled: true, keep_comments: false, formatter: Some("rustfmt") },
"javascript" => VfsConfig { enabled: true, keep_comments: true, formatter: Some("prettier") },
```

## Architecture

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  read_file   │───▶│  VFS Layer   │───▶│  minifier    │───▶ response (minified)
│  (tool)      │    │  (on read)   │    │  (tree-sitter)│
└──────────────┘    └──────────────┘    └──────────────┘

┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│  write_file  │───▶│  VFS Layer   │───▶│  formatter   │───▶ write (restored)
│  (tool)      │    │  (on write)  │    │  (rustfmt etc)│
└──────────────┘    └──────────────┘    └──────────────┘
```

## Integration Points

| File | Change |
|------|--------|
| `src/shared/minify.rs` | Replace hand-written parsers with tree-sitter queries |
| `src/tools/read_file.rs` | Route through VFS when minify=true |
| `src/tools/write_file.rs` | Optional auto-format on write |

## Token Savings

Tree-sitter minification achieves 20-50% token reduction vs raw source. That's 20-50% less context window consumed per file read, which means either cheaper sessions or longer context for the same budget.

## Dependencies

Replace `syntect` (syntax highlighting) with `tree-sitter` for the VFS path. syntect stays for TUI highlighting — they serve different purposes.

- `tree-sitter = "0.24"` — core parsing
- `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-javascript`, `tree-sitter-typescript`, `tree-sitter-go`, `tree-sitter-bash` — language grammars