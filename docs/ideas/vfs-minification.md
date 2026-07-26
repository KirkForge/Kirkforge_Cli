# VFS & Tree-Sitter Minification

**Status:** Implemented (WO 9.7, ADR-053). The agent loop's `read_file`
tool now auto-minifies files above `config.tools.minify_above_bytes`
(default 4096) before sending them to the model. The model can pass
`minify=false` to see the full raw content.

**Source note:** vix (`internal/daemon/vfs.go`) inspired the "model sees
minified, writes go through a formatter" split. KirkForge ships the
read side; the write-side formatter path (`minify_write_side`) was
already present from the earlier TUI-mentions feature.

## What shipped

`src/shared/minify/lang.rs` performs per-language minification with
string/char-literal-aware comment stripping (no tree-sitter in the hot
read path — see ADR-053 for the decision and the dep-size rationale):

- **Rust** (`minify_rust_inner`): strips `//`, `///`, `//!` line
  comments and `/* */` block comments; preserves string/char literals;
  collapses consecutive blank lines to one; optionally strips
  `#[cfg(test)]` blocks. `use` imports are kept — the model needs them.
- **TypeScript / JavaScript / JSX / TSX** (`minify_js_like`): strips
  `//` and `/* */` comments; preserves `'`, `"`, `` ` `` string
  literals; collapses blank lines.
- **Python** (`minify_python`): strips `#` comments and triple-quoted
  docstrings (`"""..."""` / `'''...'''`); collapses blank lines.
- **Go** (`minify_go`): same `//` + `/* */` stripping as JS-like.
- **C / C++ / Java** (`strip_c_style_comments`): string/char-literal
  aware `//` + `/* */` stripping.
- **Ruby / Shell / Markdown**: comment-line stripping + blank-line
  collapse (shell preserves shebang).
- JSON / YAML / TOML are returned unchanged (they have no comments to
  strip and the model needs the structure verbatim).

`src/shared/minify/mod.rs` caches results keyed by `(path, mtime)` so a
file is not re-minified across turns unless it changes.

## Read-side wiring (`src/tools/read_file.rs`)

`read_file` takes a tri-state `minify` argument:

| `minify` value   | Behavior                                             |
|------------------|------------------------------------------------------|
| `true`           | Force minify. Output carries the byte header.        |
| `false`          | Force raw. No minification, no header.               |
| omitted          | Auto: minify iff `raw_content.len() > minify_above_bytes`. |

When auto-minification triggers, the output appends:

```
[minified: N lines → M lines, use read_file with minify=false to see full content]
```

so the model learns the opt-out path from the response itself.

## Write side

When `config.tools.minify_write_side` is `true` (default `false`),
minified reads are wrapped in `<minified lang="...">...</minified>`
envelopes. `write_file` / `edit_file` strip the envelope and expand the
compressed source back to readable form via external formatters
(`rustfmt`, `black`, `prettier`, `gofmt`, ...) with a language-aware
fallback. This path is unchanged by WO 9.7; only the read-side
threshold was added.

## Config

| Field                       | Type    | Default | Env                          | TOML key             |
|-----------------------------|---------|---------|------------------------------|----------------------|
| `minify_write_side`         | bool    | `false` | `KIRKFORGE_MINIFY_WRITE_SIDE`| `minify_write_side`  |
| `minify_above_bytes`        | usize   | `4096`  | `KIRKFORGE_MINIFY_ABOVE_BYTES`| `minify_above_bytes` |

## Token savings

Comment stripping + blank-line collapse yields ~20–50% token reduction
per source file read, depending on how heavily the file is commented.
Small files (≤ 4096 bytes) are never minified, so the read path stays
free for tiny reads.

## Why not tree-sitter in the read path?

Tree-sitter is already a workspace dependency (via
`kirkforge-context-index`) and is the right tool for *symbol
extraction* (AST structure matters there). For minification, only
lexical comment/whitespace removal is needed, and the existing
string-literal-aware minifier already handles the WO's contract
correctly across all target languages. Adding tree-sitter to the
`read_file` hot path would bloat a size-optimized binary
(`opt-level=z`, `lto=true`, `codegen-units=1`) for no accuracy gain.
ADR-053 pins this decision. The original "tree-sitter-backed VFS"
target in this note is therefore **not** how the feature shipped — the
shipped form is a regex-free, hand-rolled-state-machine minifier that
is good enough and stays small.