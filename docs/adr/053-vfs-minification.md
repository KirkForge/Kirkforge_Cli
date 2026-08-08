# ADR-053: VFS minification for the agent loop `read_file` tool

- **Status:** Accepted
- **Date:** 2026-07-26

## Context

`minify_source()` (`src/shared/minify/lang.rs`) has stripped comments
and collapsed blank lines per language since the TUI-mentions feature.
It is string/char-literal aware (see `strip_c_style_comments`,
`minify_rust_inner`, `minify_python`) and handles Rust, TS/JS, Python,
Go, C/C++, Java, Ruby, shell, and markdown. Results are cached in a
VFS cache keyed by `(path, mtime)` so a file is not re-minified across
turns unless it changes.

Until WO 9.7, this minifier was wired into the TUI mentions path only
(`src/tui/commands/mentions.rs`). The agent loop's `read_file` tool
returned raw file content to the model unconditionally. Every large,
heavily-commented source file therefore cost ~20–50% more context
tokens than necessary. The decision to minify (always? threshold?
opt-out?) was never made, so `read_file` shipped without it.

## Decision

### 1. Wire minification into `read_file` with a byte threshold.

Add `minify_above_bytes: usize` (default `4096`) to `ToolConfig`
(`src/shared/config/tools.rs`). The field is configurable via TOML
(`minify_above_bytes`) and env (`KIRKFORGE_MINIFY_ABOVE_BYTES`),
mirroring the existing `minify_write_side` plumbing. Small files (≤ the
threshold) are never minified, so the read path stays free for tiny
reads and the model sees short files verbatim.

### 2. Tri-state `minify` argument on `read_file`.

The `read_file` tool schema gains a `minify` boolean parameter with no
default (omitted, not `false`):

| `minify` value | Behavior                                                   |
|----------------|------------------------------------------------------------|
| `true`         | Force minify. Output carries the byte header (unchanged).  |
| `false`        | Force raw. No minification, no header.                     |
| omitted        | Auto: minify iff `raw_content.len() > minify_above_bytes`. |

When auto-minification triggers, the output appends:

```
[minified: N lines → M lines, use read_file with minify=false to see full content]
```

so the model learns the opt-out path from the response itself — it
does not need to be told about the flag out of band.

### 3. Keep the existing string-based minifier. Do NOT add tree-sitter to the read path.

The WO brief suggested "use tree-sitter to parse + strip
comments/docstrings precisely, not regex — the tree-sitter grammars
are already in the workspace via `kf-context-index`."

This ADR rejects that approach for the read path.
`kf-context-index` already depends on `tree-sitter`,
`tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-python`,
and `tree-sitter-go` for *symbol extraction* (where AST structure
matters). For minification, only lexical comment/whitespace removal
is needed, and the existing minifier already handles the WO's contract
correctly across all target languages:

- Rust: `//`, `///`, `//!`, `/* */` stripped; strings/chars preserved;
  `#[cfg(test)]` blocks optionally stripped; `use` imports kept.
- TS/JS/JSX/TSX: `//`, `/* */` stripped; `'`/`"`/`` ` `` literals
  preserved.
- Python: `#` comments and triple-quoted docstrings stripped.
- Go: `//`, `/* */` stripped (shares the JS-like path).

Pulling tree-sitter into the `read_file` hot path would bloat a
size-optimized binary (`opt-level = "z"`, `lto = true`,
`codegen-units = 1` per `Cargo.toml`; AGENTS.md §0: "binary size
matters; a new dep must earn its place") for no accuracy gain on this
workload. The string minifier is ~500 lines of hand-rolled state
machines with no dependencies; tree-sitter + four grammars would add
measurably to the binary for the same comment-stripping result.

`docs/ideas/vfs-minification.md` originally targeted a "tree-sitter
backed VFS" with a `MinifyMode` enum and per-language `VfsConfig`. That
target is **not** how the feature shipped. The shipped form is a
regex-free, hand-rolled-state-machine minifier that is good enough and
stays small. The idea doc was updated in the same commit to
"Implemented" with an honest note about this deviation.

## Consequences

Positive:

- The model sees minified content for large files automatically, saving
  ~20–50% context tokens per large read with no model-side change.
- The model can opt out per-call (`minify=false`) when it needs the raw
  bytes (e.g. to reproduce a comment exactly in an edit).
- Small files are untouched, so the common case of reading a short
  config or source file is byte-identical to before.
- No new dependencies. The minifier, VFS cache, and write-side
  envelope expansion were all already in the tree.
- The threshold is operator-tunable (TOML + env) without a code change.

Negative:

- Auto-minification changes the byte shape of `read_file` output above
  the threshold. Any external consumer that assumed `read_file` returns
  the raw file bytes verbatim will see a header + minified body + note
  instead. The in-tree consumer (the executor) treats tool output as
  text, so this is safe; out-of-tree consumers that pin exact bytes
  would need to pass `minify=false`.
- The minifier is lexical, not AST-aware. It will not, for example,
  strip a Python docstring that is assigned to `__doc__` as a string
  expression, or elide unreachable code. The WO explicitly scoped
  minification to comments + blank lines (imports are kept), so this
  is by design, not a limitation.
- Tree-sitter-based minification (the original idea-doc target) is
  deferred indefinitely. If a future language needs AST-aware
  stripping (e.g. a language where comments nest inside expressions
  in ways the lexical minifier cannot distinguish from code), a
  follow-up ADR can revisit. The read-side seam (`minify_source` by
  extension) is the place to swap in an AST path without touching
  `read_file` again.

## Tests

- `shared::minify::lang::tests::test_minify_rust_strips_doc_and_block`
  — Rust `//`/`///`/`/* */` stripped; `use` imports kept; blank lines
  collapse.
- `shared::minify::lang::tests::test_minify_ts_strips_block_comments`
  — TS `//`/`/* */` stripped; code preserved.
- `shared::minify::lang::tests::test_minify_python_strips_docstring_and_hash`
  — Python `#` and `"""docstring"""` stripped; code preserved.
- `shared::minify::lang::tests::test_minify_go_strips_line_and_block`
  — Go `//`/`/* */` stripped; `package` and `func` preserved.
- `tools::read_file::tests::threshold_skip_small_file_not_minified`
  — small file (under threshold) with no `minify` arg returned raw.
- `tools::read_file::tests::auto_minify_large_file_emits_note`
  — large file (over threshold) with no `minify` arg auto-minified
  and the output carries the `[minified: ... lines]` note.
- `tools::read_file::tests::explicit_minify_false_returns_raw`
  — large file with `minify=false` returned raw despite the threshold.
- `shared::config::tools::tests::tool_config_defaults_match_spec` and
  `tool_config_toml_overrides_defaults` — assert the default 4096 and
  TOML override.
- `session::config::tests::test_merge_toml_minify_above_bytes` and
  `test_env_minify_above_bytes` — TOML merge and env override paths.

## Future work

- If a future language needs AST-aware comment stripping, add a
  `minify_via_treesitter` extension point in `lang.rs` rather than
  rewriting the whole module. The existing string minifier stays the
  default; the AST path is opt-in per extension.
- Surface "auto-minified" status in the TUI status bar alongside the
  existing token-budget display, so the operator can see how much
  minification is saving per session.
- Consider raising the default threshold if real-world telemetry shows
  the 4096 cutoff minifies files the model would rather see raw
  (e.g. short, dense modules with no comments where minification buys
  nothing but adds the header overhead).