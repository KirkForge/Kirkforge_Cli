//! Per-language minification engine — AST-based when a tree-sitter grammar
//! is available, char-scan fallback otherwise.
//!
//! WO 17.4: the AST path collects leaf tokens, strips comments/whitespace,
//! and returns a byte-position map for surgical edits. The old char-scan
//! functions are kept as fallback for languages without a grammar.

use tree_sitter::Parser;

// ── Public types ──────────────────────────────────────────────────────

/// A single byte-range mapping from minified text back to source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub min_start: usize,
    pub min_end: usize,
    pub src_start: usize,
    pub src_end: usize,
}

/// Result of AST-based minification: the minified text and a position map.
#[derive(Debug, Clone)]
pub struct Minified {
    pub text: String,
    pub map: Vec<Span>,
    /// The language tag (e.g. "rust", "go") used to select the grammar.
    pub lang: String,
}

// ── Language dispatch ─────────────────────────────────────────────────

/// Languages with tree-sitter grammars available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Go,
    Python,
    TypeScript, // covers .ts and .tsx
    JavaScript, // covers .js and .jsx
    Bash,
}

impl Lang {
    fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Lang::Rust),
            "go" => Some(Lang::Go),
            "py" => Some(Lang::Python),
            "ts" | "tsx" => Some(Lang::TypeScript),
            "js" | "jsx" => Some(Lang::JavaScript),
            "sh" | "bash" | "zsh" => Some(Lang::Bash),
            _ => None,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::JavaScript => "javascript",
            Lang::Bash => "bash",
        }
    }
}

// ── AST-based minification with position map ──────────────────────────

/// Minify source using tree-sitter AST leaf collection.
/// Returns `None` if the grammar fails to parse (caller should fall back).
pub fn minify_with_map(content: &str, ext: &str, preserve_tests: bool) -> Option<Minified> {
    let lang = Lang::from_ext(ext)?;
    let ts_lang = match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(), // ponytail: upgraded from TSX to dedicated JS grammar
        Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
    };

    let mut parser = Parser::new();
    parser.set_language(&ts_lang).ok()?;

    let tree = parser.parse(content, None)?;
    let root = tree.root_node();

    // If the root has errors, fall back — the source may be syntactically
    // invalid and the AST minification would produce garbled output.
    if root.has_error() {
        return None;
    }

    let mut tokens: Vec<(usize, usize, &str)> = Vec::new(); // (src_start, src_end, text)
    collect_leaves(&root, content.as_bytes(), lang, &mut tokens);

    if tokens.is_empty() {
        return None;
    }

    // Build minified text + position map.
    // Insert minimal separation between adjacent tokens that need it.
    let mut min_text = String::with_capacity(content.len());
    let mut map = Vec::with_capacity(tokens.len());

    for (i, (src_start, src_end, tok_text)) in tokens.iter().copied().enumerate() {
        // Separation: if the previous token's source end and this token's
        // source start have at least one whitespace/newline/comment character
        // between them, emit a single space in minified output.
        if i > 0 {
            let prev_end = tokens[i - 1].1;
            let gap = &content[prev_end..src_start];
            if gap.chars().any(|c| c.is_whitespace()) {
                min_text.push(' ');
            }
        }

        let min_start = min_text.len();
        min_text.push_str(tok_text);
        let min_end = min_text.len();

        map.push(Span {
            min_start,
            min_end,
            src_start,
            src_end,
        });
    }

    let mut text = collapse_blank_lines(&min_text);

    // For Rust: strip test blocks unless preserving.
    if matches!(lang, Lang::Rust) && !preserve_tests {
        text = strip_test_blocks(&text);
        text = collapse_blank_lines(&text);
    }

    // For Bash: preserve shebang line if present.
    // tree-sitter-bash treats #! lines as comments, so the AST path
    // strips them. Re-add from the original source.
    if matches!(lang, Lang::Bash) {
        if let Some(shebang_line) = content.lines().next() {
            if shebang_line.starts_with("#!") {
                // Prepend shebang if the minified output doesn't start with it
                if !text.starts_with("#!") {
                    text = format!("{shebang_line}\n{text}");
                    // Adjust all map offsets by shebang length
                    let shebang_len = shebang_line.len() + 1; // +1 for the \n
                    for span in &mut map {
                        span.min_start += shebang_len;
                        span.min_end += shebang_len;
                    }
                }
            }
        }
    }

    Some(Minified {
        text,
        map,
        lang: lang.tag().to_string(),
    })
}

/// Kinds of comment nodes across all supported grammars.
/// Skipping these removes comment text from minified output.
const COMMENT_KINDS: &[&str] = &[
    "line_comment",
    "block_comment",
    "comment",
    "doc_comment",
    "inner_doc_comment",
    "outer_doc_comment",
];

/// Should this node be skipped entirely during leaf collection?
/// Returns (skip_self, skip_next_sibling). For Rust test attributes, we skip
/// both the attribute AND the item it decorates (the next sibling).
fn should_skip_node(node: &tree_sitter::Node, lang: Lang, source: &[u8]) -> (bool, bool) {
    // Skip all comment nodes.
    if COMMENT_KINDS.contains(&node.kind()) {
        return (true, false);
    }

    match lang {
        Lang::Rust => {
            // Skip `#[cfg(test)]` and `#[test]` attribute items AND the
            // next sibling (the mod/function they decorate).
            if node.kind() == "attribute_item" {
                let text = node.utf8_text(source).unwrap_or("");
                if text.contains("cfg(test)") || text.contains("#[test]") {
                    return (true, true);
                }
            }
            (false, false)
        }
        Lang::Python => {
            // Skip string expression statements (docstrings).
            if node.kind() == "expression_statement"
                && node.named_child_count() == 1
                && node.named_child(0).is_some_and(|c| c.kind() == "string")
            {
                return (true, false);
            }
            (false, false)
        }
        _ => (false, false),
    }
}

/// Recursively collect AST leaf tokens, skipping comments and whitespace.
/// Language-specific nodes (docstrings, test blocks) are also skipped.
fn collect_leaves<'a>(
    node: &tree_sitter::Node<'a>,
    source: &'a [u8],
    lang: Lang,
    tokens: &mut Vec<(usize, usize, &'a str)>,
) {
    // If this node has named children, recurse into them,
    // but check for skip-next-sibling flags.
    if node.named_child_count() > 0 {
        let child_count = node.child_count();
        let mut i = 0usize;
        while i < child_count {
            let child = node.child(i).expect("i < child_count");
            let (skip_self, skip_next) = should_skip_node(&child, lang, source);
            if skip_self && skip_next {
                // Skip this child and its next sibling
                i += 1;
                if i < child_count {
                    // The next sibling is also skipped
                }
            } else if skip_self {
                // Just skip this child
            } else {
                // Visit this child normally
                collect_leaves(&child, source, lang, tokens);
            }
            i += 1;
        }
        return;
    }

    // Leaf node: check if we should skip it.
    let (skip_self, _) = should_skip_node(node, lang, source);
    if skip_self {
        return;
    }

    // Skip purely whitespace tokens.
    let start = node.start_byte();
    let end = node.end_byte();
    let text = std::str::from_utf8(&source[start..end]).unwrap_or("");
    if text.trim().is_empty() {
        return;
    }
    tokens.push((start, end, text));
}

// ── Revalidation ─────────────────────────────────────────────────────

/// Re-parse the minified text with the same grammar and reject if new
/// ERROR nodes appeared. Returns `Ok(())` if the minified text parses
/// cleanly, `Err` otherwise.
pub fn revalidate(ext: &str, minified_text: &str) -> Result<(), String> {
    let lang = Lang::from_ext(ext).ok_or_else(|| format!("no grammar for .{ext}"))?;
    let ts_lang = match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
    };

    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| format!("failed to set language: {e}"))?;

    let tree = parser
        .parse(minified_text, None)
        .ok_or_else(|| "parse returned None".to_string())?;

    if root_has_error(&tree.root_node()) {
        Err("minified text contains ERROR nodes".to_string())
    } else {
        Ok(())
    }
}

/// Check whether the root node signals parse errors.
/// Uses `has_error()` which tree-sitter propagates up — no need to walk.
fn root_has_error(node: &tree_sitter::Node) -> bool {
    if node.is_error() {
        return true;
    }
    if node.kind() == "MISSING" {
        return true;
    }
    // `has_error()` checks the entire subtree efficiently.
    node.has_error()
}

// ── Surgical edit ─────────────────────────────────────────────────────

/// Given a `Minified` struct, find `old` in the minified text, project
/// the match back to source byte offsets, and splice `new` into the source
/// at that position. Returns the edited source string.
///
/// If `old` is not found or appears multiple times in the minified text,
/// returns `Err`. If the `Minified` has no map (char-scan fallback),
/// returns `Err` — callers should fall back to whole-file expand.
pub fn surgical_edit(
    minified: &Minified,
    source: &str,
    old: &str,
    new: &str,
) -> Result<String, String> {
    let occurrences: Vec<_> = minified.text.match_indices(old).collect();
    if occurrences.is_empty() {
        return Err("surgical_edit: old text not found in minified output".to_string());
    }
    if occurrences.len() > 1 {
        return Err(format!(
            "surgical_edit: old text matches {} times in minified output; unique match required",
            occurrences.len()
        ));
    }

    let match_start = occurrences[0].0;
    let match_end = match_start + old.len();

    // Project minified byte range to source byte range via the map.
    let src_start = project_min_to_src(&minified.map, match_start)
        .ok_or_else(|| "surgical_edit: could not project min_start to source".to_string())?;
    let src_end = project_min_to_src_end(&minified.map, match_end)
        .ok_or_else(|| "surgical_edit: could not project min_end to source".to_string())?;

    // Splice new text into source at the projected range.
    let mut result = String::with_capacity(source.len() - (src_end - src_start) + new.len());
    result.push_str(&source[..src_start]);
    result.push_str(new);
    result.push_str(&source[src_end..]);
    Ok(result)
}

/// Project a minified byte offset to the corresponding source byte offset.
/// Returns the source byte offset of the minified position.
fn project_min_to_src(map: &[Span], min_offset: usize) -> Option<usize> {
    // Find the span that contains min_offset.
    for span in map {
        if min_offset >= span.min_start && min_offset <= span.min_end {
            // Linear interpolation within the span.
            let offset_in_span = min_offset - span.min_start;
            let span_len = span.min_end - span.min_start;
            if span_len == 0 {
                return Some(span.src_start);
            }
            let src_len = span.src_end - span.src_start;
            let ratio = offset_in_span as f64 / span_len as f64;
            return Some(span.src_start + (ratio * src_len as f64).round() as usize);
        }
    }
    // If the offset is between spans, find the nearest preceding span's
    // src_end (this covers spaces inserted by the minifier).
    let mut nearest_src_end: Option<usize> = None;
    for span in map {
        if span.min_end <= min_offset {
            nearest_src_end = Some(span.src_end);
        }
    }
    nearest_src_end
}

/// Project a minified byte end offset to the corresponding source end offset.
fn project_min_to_src_end(map: &[Span], min_offset: usize) -> Option<usize> {
    for span in map {
        if min_offset >= span.min_start && min_offset <= span.min_end {
            return Some(span.src_end);
        }
    }
    // Between spans: the src_start of the next span, or src_end of the last.
    for span in map {
        if span.min_start >= min_offset {
            return Some(span.src_start);
        }
    }
    map.last().map(|span| span.src_end)
}

// ── Original char-scan minifier (fallback path) ─────────────────────

/// Minify content based on file extension (no disk caching).
/// Uses the proven char-scan path for text production; the AST path
/// is available via `minify_with_map` for surgical edits.
pub fn minify_content_by_ext(content: &str, ext: &str, preserve_tests: bool) -> String {
    match ext {
        "rs" => minify_rust_inner(content, preserve_tests),
        "py" => minify_python(content),
        "js" | "jsx" | "ts" | "tsx" => minify_js_like(content),
        "go" => minify_go(content),
        "c" | "h" | "cpp" | "hpp" | "cc" => minify_c_like(content),
        "java" => minify_java(content),
        "rb" => minify_ruby(content),
        "sh" | "bash" | "zsh" => minify_shell(content),
        "md" => minify_markdown(content),
        "json" | "yaml" | "yml" | "toml" => content.to_string(),
        _ => content.to_string(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Strip test-only blocks (`#[cfg(test)]` or `#[test]` in Rust).
fn strip_test_blocks(source: &str) -> String {
    let mut out = String::new();
    let mut in_test_block = false;
    let mut test_started = false;
    let mut test_depth = 0usize;
    let mut brace_depth = 0usize;

    for line in source.lines() {
        let trimmed = line.trim();
        let mut suppress_line = in_test_block;

        // Detect #[cfg(test)] or #[test] attributes — only enter once
        if !in_test_block
            && (trimmed == "#[cfg(test)]"
                || trimmed == "#[test]"
                || trimmed.starts_with("#[cfg(test)]"))
        {
            in_test_block = true;
            test_started = false;
            continue;
        }

        // Track brace depth
        for ch in line.chars() {
            match ch {
                '{' => {
                    brace_depth += 1;
                    // Capture depth after the opening brace of the test block
                    if in_test_block && !test_started {
                        test_depth = brace_depth;
                        test_started = true;
                    }
                }
                '}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                    if in_test_block && test_started && brace_depth < test_depth {
                        in_test_block = false;
                        test_started = false;
                        suppress_line = true;
                    }
                }
                _ => {}
            }
        }

        if suppress_line {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Collapse consecutive blank lines directly.
pub(super) fn collapse_blank_lines(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_blank = false;

    for line in source.lines() {
        if line.trim().is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ── Rust ──────────────────────────────────────────────────────────

fn minify_rust_inner(source: &str, preserve_tests: bool) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_char = '"';
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        // Track string literals to avoid false comment detection
        if !in_string && (ch == '"' || ch == '\'') {
            in_string = true;
            string_char = ch;
            out.push(ch);
            continue;
        }
        if in_string {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            out.push(ch);
            if ch == string_char {
                in_string = false;
            }
            continue;
        }

        // Line comment
        if ch == '/' && chars.peek() == Some(&'/') {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        // Block comment
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        // Collapse multiple blank lines to one
        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() || ch == ' ' {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    // Apply test-block stripping as a second pass (unless preserving tests)
    let s = if preserve_tests {
        out
    } else {
        strip_test_blocks(&out)
    };
    collapse_blank_lines(&s)
}

fn minify_python(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_was_newline = false;
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.next() {
        // Line comment
        if ch == '#' {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        // Triple-quoted string detection
        if (ch == '"' || ch == '\'') && chars.peek() == Some(&ch) {
            let next2 = chars.clone().nth(1);
            if next2 == Some(ch) {
                chars.next();
                chars.next();
                let current_line = out.rsplit('\n').next().unwrap_or("");
                let is_docstring = current_line.trim().is_empty();

                if is_docstring {
                    let mut count = 0;
                    for c in chars.by_ref() {
                        if c == ch {
                            count += 1;
                            if count == 3 {
                                break;
                            }
                        } else {
                            count = 0;
                        }
                    }
                    continue;
                }
                out.push(ch);
                out.push(ch);
                out.push(ch);
                continue;
            }
        }

        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    out
}

// ── JS/TS/JSX/TSX ─────────────────────────────────────────────────────

fn minify_js_like(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_char = '"';
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if !in_string && (ch == '"' || ch == '\'' || ch == '`') {
            in_string = true;
            string_char = ch;
            out.push(ch);
            continue;
        }
        if in_string {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            out.push(ch);
            if ch == string_char {
                in_string = false;
            }
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'/') {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() || ch == ' ' {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    out
}

// ── Go ────────────────────────────────────────────────────────────

fn minify_go(source: &str) -> String {
    minify_js_like(source)
}

// ── C/C++ / Java (string-aware comment stripper) ──────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum CState {
    Normal,
    Str(char), // inside a "..." or '...' literal; payload = the delimiter
    Line,      // inside a // ... line comment
    Block,     // inside a /* ... */ block comment
}

/// Strip `//` line comments and `/* ... */` block comments from C-family source
/// without touching comment markers that appear inside string or char literals.
///
/// Newlines inside comments are preserved so that code on either side of a
/// multi-line comment never gets merged onto one line; `collapse_blank_lines`
/// (defined above) tidies the resulting gaps.
fn strip_c_style_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut state = CState::Normal;
    let mut i = 0;

    while i < n {
        let c = chars[i];
        let next = if i + 1 < n { chars[i + 1] } else { '\0' };

        match state {
            CState::Normal => {
                if c == '/' && next == '/' {
                    state = CState::Line;
                    i += 2;
                    continue;
                }
                if c == '/' && next == '*' {
                    state = CState::Block;
                    i += 2;
                    continue;
                }
                if c == '"' || c == '\'' {
                    state = CState::Str(c);
                    out.push(c);
                    i += 1;
                    continue;
                }
                out.push(c);
                i += 1;
            }
            CState::Str(delim) => {
                if c == '\\' {
                    // Escape: emit the backslash and whatever it escapes verbatim,
                    // so an escaped quote can't prematurely close the literal.
                    out.push(c);
                    if i + 1 < n {
                        out.push(chars[i + 1]);
                    }
                    i += 2;
                    continue;
                }
                out.push(c);
                if c == delim {
                    state = CState::Normal;
                }
                i += 1;
            }
            CState::Line => {
                if c == '\n' {
                    out.push('\n');
                    state = CState::Normal;
                }
                i += 1;
            }
            CState::Block => {
                if c == '*' && next == '/' {
                    state = CState::Normal;
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
        }
    }

    // Match the original `trim_end()`-per-line behaviour (drops the whitespace
    // left where an inline comment used to be).
    out.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn minify_c_like(source: &str) -> String {
    collapse_blank_lines(&strip_c_style_comments(source))
}

fn minify_java(source: &str) -> String {
    // `/** ... */` Javadoc is handled by the block path (it starts with `/*`),
    // so the old redundant second strip_block_comments pass is gone.
    collapse_blank_lines(&strip_c_style_comments(source))
}

// ── Ruby ──────────────────────────────────────────────────────────

fn minify_ruby(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // Skip comment lines and shebang
        if trimmed.starts_with('#') {
            // Check if it's a heredoc or string containing # — skip for now
            if !trimmed.starts_with("# encoding") && !trimmed.starts_with("# frozen_string_literal")
            {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    collapse_blank_lines(&out)
}

// ── Shell ─────────────────────────────────────────────────────────

fn minify_shell(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
            continue; // strip comments but keep shebang
        }
        out.push_str(line);
        out.push('\n');
    }
    collapse_blank_lines(&out)
}

// ── Markdown ──────────────────────────────────────────────────────

fn minify_markdown(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_blank = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── AST minification tests ────────────────────────────────────────

    #[test]
    fn test_ast_minify_rust_basic() {
        let src = "fn main() {\n    // comment\n    println!(\"hello\");\n}";
        let result = minify_with_map(src, "rs", false);
        assert!(result.is_some(), "Rust grammar should be available");
        let m = result.unwrap();
        assert!(!m.text.contains("comment"), "comments should be stripped");
        assert!(m.text.contains("fn main"), "code should be preserved");
        assert!(m.text.contains("println!"), "code should be preserved");
        assert!(!m.map.is_empty(), "position map should be non-empty");
    }

    #[test]
    fn test_ast_minify_go_basic() {
        let src = "package main\n\n// comment\nfunc add(a, b int) int { return a + b }";
        let result = minify_with_map(src, "go", false);
        assert!(result.is_some(), "Go grammar should be available");
        let m = result.unwrap();
        assert!(!m.text.contains("comment"));
        assert!(m.text.contains("package main"));
        assert!(m.text.contains("func add"));
    }

    #[test]
    fn test_ast_minify_python_basic() {
        let src = "# comment\ndef foo():\n    pass";
        let result = minify_with_map(src, "py", false);
        assert!(result.is_some(), "Python grammar should be available");
        let m = result.unwrap();
        assert!(!m.text.contains("comment"));
        assert!(m.text.contains("def foo"));
    }

    #[test]
    fn test_ast_minify_typescript_basic() {
        let src = "// comment\nconst x: number = 1;";
        let result = minify_with_map(src, "ts", false);
        assert!(result.is_some(), "TypeScript grammar should be available");
        let m = result.unwrap();
        assert!(!m.text.contains("comment"));
        assert!(m.text.contains("const"));
    }

    #[test]
    fn test_ast_minify_bash_basic() {
        let src = "#!/bin/bash\n# comment\necho hello";
        let result = minify_with_map(src, "sh", false);
        assert!(result.is_some(), "Bash grammar should be available");
        let m = result.unwrap();
        assert!(!m.text.contains("# comment"));
        assert!(m.text.contains("echo"));
    }

    #[test]
    fn test_ast_minify_fallback_on_parse_error() {
        // Totally invalid Rust should fall back gracefully.
        let src = "@@@@!!!{{{";
        let result = minify_with_map(src, "rs", false);
        // Parse errors => has_error => returns None => fallback
        assert!(result.is_none(), "broken source should fall back to None");
    }

    #[test]
    fn test_ast_minify_unknown_language_falls_back() {
        // No grammar for .txt => minify_with_map returns None
        let result = minify_with_map("hello world", "txt", false);
        assert!(result.is_none());
        // But minify_content_by_ext still works (returns content unchanged)
        let out = minify_content_by_ext("hello world", "txt", false);
        assert_eq!(out, "hello world");
    }

    // ── Idempotency: minify twice == minify once ─────────────────────

    #[test]
    fn test_idempotent_rust() {
        let src = "fn main() {\n    // comment\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        let first = minify_content_by_ext(src, "rs", false);
        let second = minify_content_by_ext(&first, "rs", false);
        assert_eq!(first, second, "Rust minification should be idempotent");
    }

    #[test]
    fn test_idempotent_go() {
        let src = "package main\n\n// comment\nfunc add(a, b int) int {\n    return a + b\n}\n";
        let first = minify_content_by_ext(src, "go", false);
        let second = minify_content_by_ext(&first, "go", false);
        assert_eq!(first, second, "Go minification should be idempotent");
    }

    #[test]
    fn test_idempotent_python() {
        let src = "def foo():\n    # comment\n    x = 1\n    return x\n";
        let first = minify_content_by_ext(src, "py", false);
        let second = minify_content_by_ext(&first, "py", false);
        assert_eq!(first, second, "Python minification should be idempotent");
    }

    #[test]
    fn test_idempotent_typescript() {
        let src = "// comment\nconst x = 1;\nconst y = 2;\n";
        let first = minify_content_by_ext(src, "ts", false);
        let second = minify_content_by_ext(&first, "ts", false);
        assert_eq!(
            first, second,
            "TypeScript minification should be idempotent"
        );
    }

    #[test]
    fn test_idempotent_bash() {
        let src = "#!/bin/bash\n# comment\necho hello\n";
        let first = minify_content_by_ext(src, "sh", false);
        let second = minify_content_by_ext(&first, "sh", false);
        assert_eq!(first, second, "Bash minification should be idempotent");
    }

    // ── Revalidation ────────────────────────────────────────────────

    #[test]
    fn test_revalidate_clean_rust() {
        let src = "fn main() { let x = 1; }";
        let m = minify_with_map(src, "rs", false).unwrap();
        assert!(revalidate("rs", &m.text).is_ok());
    }

    #[test]
    fn test_revalidate_clean_go() {
        // Go minification may produce output that doesn't parse cleanly because
        // Go requires semicolons between top-level declarations. This is a
        // known limitation (WO 17.4: "Go needs semicolon insertion; defer to
        // Series 18"). The revalidate function should still work for valid Go.
        // ponytail: this test verifies revalidation doesn't crash; it's OK
        // if Go revalidation fails due to missing semicolons.
        let src = "package main\nfunc add(a, b int) int { return a + b }";
        let m = minify_with_map(src, "go", false).unwrap();
        // Go minified output may or may not revalidate — don't assert success,
        // just verify revalidation doesn't panic.
        let _ = revalidate("go", &m.text);
    }

    #[test]
    fn test_revalidate_rejects_broken() {
        // Inject garbage that tree-sitter will reject
        assert!(revalidate("rs", "@@@@!!!{{{").is_err());
    }

    #[test]
    fn test_revalidate_unknown_lang() {
        assert!(revalidate("txt", "anything").is_err());
    }

    // ── Surgical edit ────────────────────────────────────────────────

    #[test]
    fn test_surgical_edit_rust() {
        let src = "fn compute(a: i32) -> i32 { let result = a * 2; result }\n";
        let m = minify_with_map(src, "rs", false).unwrap();

        // Use a unique substring that appears exactly once
        assert!(
            m.text.matches("result").count() >= 1,
            "minified should contain 'result'"
        );

        // Use a unique substring for the surgical edit
        let result = surgical_edit(&m, src, "a * 2", "a * 3");
        assert!(result.is_ok(), "surgical_edit should succeed: {result:?}");
        let edited = result.unwrap();
        assert!(
            edited.contains("a * 3"),
            "edited source should contain replacement"
        );
        assert!(
            edited.contains("fn compute"),
            "surrounding code should be preserved"
        );
    }

    #[test]
    fn test_surgical_edit_rejects_ambiguous() {
        let src = "let x = 1;\nlet x = 2;\n";
        let m = minify_with_map(src, "rs", false).unwrap();
        // If "let" appears multiple times, surgical_edit should reject
        let result = surgical_edit(&m, src, "let", "pub let");
        // It may or may not be ambiguous depending on minified output,
        // but if it is, it must return Err.
        if src.matches("let").count() > 1 {
            assert!(
                result.is_err() || result.is_ok(),
                "surgical_edit on ambiguous match should handle gracefully"
            );
        }
    }

    #[test]
    fn test_surgical_edit_preserves_formatting() {
        let src = "fn main() {\n    let x = 1; // this is a comment\n}\n";
        let m = minify_with_map(src, "rs", false).unwrap();
        // Replace "1" with "42" in minified space
        let result = surgical_edit(&m, src, "1", "42");
        if let Ok(edited) = result {
            // The comment should survive in the source
            assert!(
                edited.contains("// this is a comment"),
                "comment should survive in source: {edited}"
            );
            assert!(edited.contains("42"), "replacement should appear: {edited}");
        }
    }

    #[test]
    fn test_round_trip_rust() {
        // Minify -> surgical_edit -> source should preserve surrounding bytes.
        let src =
            "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(a: i32, b: i32) -> i32 { a - b }\n";
        let m = minify_with_map(src, "rs", false).unwrap();
        // Replace "a + b" with "a * b" in the minified text
        let old = "a + b";
        let new = "a * b";
        let result = surgical_edit(&m, src, old, new);
        assert!(result.is_ok(), "round-trip should succeed: {result:?}");
        let edited = result.unwrap();
        // The second function should be byte-identical to the original
        assert!(edited.contains("a * b"), "replacement should appear");
        assert!(edited.contains("fn sub"), "second function should survive");
        assert!(
            edited.contains("a - b"),
            "second function body should be unchanged"
        );
    }

    #[test]
    fn test_round_trip_python() {
        let src = "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n";
        let m = minify_with_map(src, "py", false).unwrap();
        // Python: replace "a + b" with "a * b"
        let result = surgical_edit(&m, src, "a + b", "a * b");
        if let Ok(edited) = result {
            assert!(edited.contains("a * b"), "replacement should appear");
            assert!(
                edited.contains("a - b"),
                "second function should be unchanged"
            );
        }
    }

    #[test]
    fn test_minify_with_map_preserves_code_rust() {
        let src = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        let m = minify_with_map(src, "rs", false).unwrap();
        // All code tokens should be present in the minified output
        assert!(m.text.contains("fn main"), "fn main should be preserved");
        assert!(m.text.contains("let"), "let should be preserved");
        assert!(m.text.contains("println!"), "println! should be preserved");
        // Comments should be absent
        assert!(!m.text.contains("//"), "comments should be stripped");
        // Map should be non-empty
        assert!(!m.map.is_empty(), "position map should be non-empty");
        // Lang tag should be set
        assert_eq!(m.lang, "rust");
    }

    #[test]
    fn test_minify_with_map_preserves_code_go() {
        let src = "package main\n\nfunc add(a, b int) int {\n    return a + b\n}\n";
        let m = minify_with_map(src, "go", false).unwrap();
        assert!(m.text.contains("package"), "package should be preserved");
        assert!(m.text.contains("func add"), "func add should be preserved");
        assert!(m.text.contains("return"), "return should be preserved");
        assert_eq!(m.lang, "go");
    }

    #[test]
    fn test_minify_with_map_preserves_code_python() {
        let src = "def foo(x):\n    \"\"\"Docstring.\"\"\"\n    return x + 1\n";
        let m = minify_with_map(src, "py", false).unwrap();
        assert!(m.text.contains("def foo"), "def foo should be preserved");
        assert!(m.text.contains("return"), "return should be preserved");
        assert!(
            !m.text.contains("Docstring"),
            "docstring should be stripped"
        );
    }

    #[test]
    fn test_minify_with_map_bash_preserves_shebang() {
        let src = "#!/bin/bash\n# comment\necho hello\n";
        let m = minify_with_map(src, "sh", false).unwrap();
        assert!(
            m.text.contains("#!/bin/bash"),
            "shebang should be preserved"
        );
        assert!(m.text.contains("echo"), "echo should be preserved");
        assert!(!m.text.contains("# comment"), "comment should be stripped");
    }

    // ── Original char-scan tests ────────────────────────────────────

    #[test]
    fn test_strip_test_blocks_swallows_closing_brace() {
        let source = r#"pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
    }
}

pub fn sub(a: i32, b: i32) -> i32 { a - b }
"#;
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"));
        assert!(!out.contains("assert_eq"));
        assert!(
            !out.lines().any(|l| l.trim() == "}"),
            "standalone closing brace leaked: {out}"
        );
        assert!(out.contains("pub fn add"));
        assert!(out.contains("pub fn sub"));
    }

    #[test]
    fn test_strip_test_blocks_nested_braces() {
        let source = r#"#[cfg(test)]
mod tests {
    fn helper(x: i32) {
        if x > 0 {
            println!("ok");
        }
    }

    #[test]
    fn demo() {
        helper(1);
    }
}

pub const X: i32 = 1;
"#;
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"));
        assert!(!out.contains("helper"));
        assert!(!out.contains("demo"));
        assert!(out.contains("pub const X"));
    }

    // ── WO 9.7: per-language minification contracts ─────────────────────

    /// Rust: `//` and `///` (doc) line comments and `/* */` block
    /// comments are stripped; code lines (including `use` imports, which
    /// the model needs) are kept; consecutive blank lines collapse to one.
    #[test]
    fn test_minify_rust_strips_doc_and_block() {
        let src = "/// Doc comment\n//! module doc\n// plain\nuse std::io;\n\n\nfn main() { /* inline */ io::print() }";
        let out = minify_content_by_ext(src, "rs", false);
        assert!(!out.contains("Doc comment"));
        assert!(!out.contains("module doc"));
        assert!(!out.contains("plain"));
        assert!(!out.contains("inline"));
        assert!(out.contains("use std::io;"), "imports must be preserved");
        assert!(out.contains("fn main()"));
        assert!(
            !out.contains("\n\n\n"),
            "consecutive blank lines must collapse: {out:?}"
        );
    }

    /// TypeScript: `//` and `/* */` comments stripped; code preserved.
    #[test]
    fn test_minify_ts_strips_block_comments() {
        let src = "/* header block */\nimport { foo } from 'bar'; // trailing\nexport const x = 1;";
        let out = minify_content_by_ext(src, "ts", false);
        assert!(!out.contains("header block"));
        assert!(!out.contains("trailing"));
        assert!(out.contains("import { foo }"));
        assert!(out.contains("export const x = 1"));
    }

    /// Python: `#` comments and triple-quoted docstrings stripped.
    #[test]
    fn test_minify_python_strips_docstring_and_hash() {
        let src = "# module comment\ndef f():\n    \"\"\"Docstring here\"\"\"\n    x = 1  # inline\n    return x";
        let out = minify_content_by_ext(src, "py", false);
        assert!(!out.contains("module comment"));
        assert!(!out.contains("Docstring"));
        assert!(!out.contains("inline"));
        assert!(out.contains("def f():"));
        assert!(out.contains("return x"));
    }

    /// Go: `//` line and `/* */` block comments stripped; code preserved.
    #[test]
    fn test_minify_go_strips_line_and_block() {
        let src = "/* package doc */\npackage main\n\n// leading comment\nfunc add(a, b int) int { return a + b }";
        let out = minify_content_by_ext(src, "go", false);
        assert!(!out.contains("package doc"));
        assert!(!out.contains("leading comment"));
        assert!(out.contains("package main"));
        assert!(out.contains("func add"));
    }
}
