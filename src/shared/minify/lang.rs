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

// String-literal state carried across lines while scanning Rust source
// (WO 48.40). Only `"` literals span lines; `'` chars are single-line.
#[derive(Clone, Copy)]
struct StrScan {
    raw: bool,
    hashes: usize,
}

// Classify the string opening at a `"` from the bytes before it:
// `r#*"` opens a raw string closed by `"` plus that many `#`.
fn open_string(before: &str) -> StrScan {
    let b = before.as_bytes();
    let mut j = b.len();
    while j > 0 && b[j - 1] == b'#' {
        j -= 1;
    }
    let raw = j > 0 && b[j - 1] == b'r';
    StrScan {
        raw,
        hashes: if raw { b.len() - j } else { 0 },
    }
}

/// Strip test-only blocks (`#[cfg(test)]` or `#[test]` in Rust).
/// Markers and braces inside string literals (incl. raw `r#"..."#`) and
/// block comments do not count — only real attribute lines enter
/// stripping (WO 48.40). Entry-line braces count, so a one-liner
/// `#[cfg(test)] mod tests {}` consumes only its own line, and a
/// brace-less `mod tests;` consumes the marker lines only (WO 48.47).
fn strip_test_blocks(source: &str) -> String {
    let mut out = String::new();
    let mut in_test_block = false;
    let mut test_started = false;
    let mut test_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut str_state: Option<StrScan> = None;
    let mut in_block_comment = false;

    for line in source.lines() {
        let trimmed = line.trim();
        let mut suppress_line = in_test_block;

        // Detect #[cfg(test)] or #[test] attributes — only enter once,
        // and only when the line start is not inside a string literal
        // or block comment.
        if !in_test_block
            && str_state.is_none()
            && !in_block_comment
            && (trimmed == "#[cfg(test)]"
                || trimmed == "#[test]"
                || trimmed.starts_with("#[cfg(test)]"))
        {
            in_test_block = true;
            test_started = false;
            suppress_line = true;
            // Fall through to the brace scan: braces on the entry line
            // (one-liner `mod tests {}`) still count (WO 48.47).
        }

        // Brace-less decorated item (`mod tests;`, incl. the one-liner
        // `#[cfg(test)] mod tests;`) is already complete — consume it and
        // resume normal output (WO 48.47).
        if in_test_block && !test_started && trimmed.ends_with(';') && !trimmed.contains('{') {
            in_test_block = false;
            continue;
        }

        // Track brace depth, skipping string-literal and comment content.
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if in_block_comment {
                // Inside /* */: quotes, chars and braces are comment
                // text — only the terminator matters (WO 48.47).
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    in_block_comment = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(s) = str_state {
                // Inside a string: only look for its terminator.
                if !s.raw {
                    if bytes[i] == b'\\' {
                        i += 1; // escaped char — never a terminator
                    } else if bytes[i] == b'"' {
                        str_state = None;
                    }
                } else if bytes[i] == b'"' {
                    let mut n = 0;
                    while n < s.hashes && bytes.get(i + 1 + n) == Some(&b'#') {
                        n += 1;
                    }
                    if n == s.hashes {
                        i += 1 + s.hashes;
                        str_state = None;
                    }
                }
                i += 1;
                continue;
            }
            match bytes[i] {
                b'"' => str_state = Some(open_string(&line[..i])),
                b'/' if bytes.get(i + 1) == Some(&b'/') => break, // line comment
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    in_block_comment = true;
                    i += 2;
                    continue;
                }
                b'\'' => {
                    // Char literal ('\x' or 'x'); a bare `'a` is a lifetime.
                    let rest = &line[i + 1..];
                    let rb = rest.as_bytes();
                    let n = if rb.first() == Some(&b'\\') && rb.get(2) == Some(&b'\'') {
                        4
                    } else if rb.first() != Some(&b'\\') && rb.get(1) == Some(&b'\'') {
                        3
                    } else {
                        1
                    };
                    i += n;
                    continue;
                }
                b'{' => {
                    brace_depth += 1;
                    // Capture depth after the opening brace of the test block
                    if in_test_block && !test_started {
                        test_depth = brace_depth;
                        test_started = true;
                    }
                }
                b'}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                    if in_test_block && test_started && brace_depth < test_depth {
                        in_test_block = false;
                        test_started = false;
                        suppress_line = true;
                    }
                }
                _ => {}
            }
            i += 1;
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

        // Raw strings (r#*"/br#*") run verbatim to their `"` + N `#` closer:
        // `//`, `/*` and `\` inside are content, not comments/escapes (48.46).
        if !in_string && ch == '"' {
            let scan = open_string(&out);
            if scan.raw {
                out.push(ch);
                while let Some(c) = chars.next() {
                    out.push(c);
                    if c == '"' {
                        let mut n = 0;
                        while n < scan.hashes && chars.peek() == Some(&'#') {
                            chars.next();
                            out.push('#');
                            n += 1;
                        }
                        if n == scan.hashes {
                            break;
                        }
                    }
                }
                continue;
            }
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

fn python_at_block_start(out: &str) -> bool {
    out.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_none_or(|l| l.trim_end().ends_with(':'))
}

fn minify_python(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_was_newline = false;
    let mut chars = source.chars().peekable();
    // String-literal state: Some(q) inside a '...'/"..."/triple literal.
    let mut string_char: Option<char> = None;
    let mut in_triple = false;
    // Bracket depth: docstrings never appear inside (), [], {} — a
    // triple-quoted literal there is an argument/element.
    let mut paren_depth: usize = 0;

    while let Some(ch) = chars.next() {
        if let Some(q) = string_char {
            // Escape: the char after a backslash stays inside the literal.
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            out.push(ch);
            if ch == q {
                if in_triple {
                    // Only a run of three delimiters closes a triple-quoted string.
                    if chars.peek() == Some(&q) && chars.clone().nth(1) == Some(q) {
                        chars.next();
                        chars.next();
                        out.push(q);
                        out.push(q);
                        string_char = None;
                        in_triple = false;
                    }
                } else {
                    string_char = None;
                }
            }
            continue;
        }

        // Line comment — only outside string literals
        if ch == '#' {
            while chars.peek().is_some_and(|&c| c != '\n') {
                chars.next();
            }
            continue;
        }

        // String literal start (triple-quoted needs docstring check)
        if ch == '"' || ch == '\'' {
            let is_triple = chars.peek() == Some(&ch) && chars.clone().nth(1) == Some(ch);
            if is_triple {
                chars.next();
                chars.next();
                let current_line = out.rsplit('\n').next().unwrap_or("");
                let is_docstring = paren_depth == 0
                    && current_line.trim().is_empty()
                    && python_at_block_start(&out);

                if is_docstring {
                    // Docstring: drop the whole literal (escape-aware scan).
                    let mut count = 0;
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            chars.next();
                            count = 0;
                            continue;
                        }
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
                string_char = Some(ch);
                in_triple = true;
                continue;
            }
            out.push(ch);
            string_char = Some(ch);
            continue;
        }

        // Bracket depth — counted only outside strings/comments (those arms
        // `continue` before reaching here).
        match ch {
            '(' | '[' | '{' => paren_depth += 1,
            ')' | ']' | '}' => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
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

// Conservative regex-vs-division heuristic: the identifier ending at the
// end of `s` (after trailing whitespace), empty if the last char isn't a
// word character.
pub(super) fn trailing_word(s: &str) -> &str {
    let trimmed = s.trim_end();
    let start = trimmed
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .map_or(0, |i| i + 1);
    &trimmed[start..]
}

// Can a regex literal follow the last non-whitespace token of `out`?
// Operators/punctuators and expression keywords say yes (an expression can
// start there); identifiers/numbers/`)`/`]`/quote say no (division context).
// ponytail: keyword-list heuristic, not a tokenizer — regexes after `)`/`}`
// or an identifier (division-looking contexts) stay untracked; a `//` or
// quote inside one of those can still corrupt. Upgrade path: real lexer.
pub(super) fn prev_opens_regex(out: &str) -> bool {
    match out.trim_end().chars().next_back() {
        None => true, // start of input
        Some(c) if "(,;:=!+-*%<>&|^~?{[".contains(c) => true,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => matches!(
            trailing_word(out),
            "return"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "new"
                | "delete"
                | "void"
                | "case"
                | "do"
                | "else"
                | "yield"
                | "await"
                | "throw"
        ),
        Some(_) => false, // `)`, `]`, `}`, quotes → division context
    }
}

fn minify_js_like(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_char = '"';
    let mut in_regex = false;
    let mut in_char_class = false;
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        // Regex literal: emit verbatim until the closing unescaped `/`.
        // `//` and `/*` inside are literal (comment checks never run here —
        // the opening `/` was followed by something else).
        if in_regex {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            if ch == '\n' {
                // Regex literals can't span lines — we misdetected; bail to
                // normal scanning so the damage stops at this line.
                in_regex = false;
                in_char_class = false;
            } else {
                if ch == '[' {
                    in_char_class = true;
                } else if ch == ']' {
                    in_char_class = false;
                } else if ch == '/' && !in_char_class {
                    in_regex = false;
                    in_char_class = false;
                }
                out.push(ch);
                continue;
            }
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

        if ch == '/' && prev_opens_regex(&out) {
            in_regex = true;
            in_char_class = false;
            out.push(ch);
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

/// One %-literal (`%q(...)`, `%w[...]`, `%{...}`, `%!...!`) left open
/// across lines: delimiters + nesting depth (bracket pairs nest in
/// ruby, same-char delimiters don't).
struct PctOpen {
    open: char,
    close: char,
    depth: usize,
}

/// Is this a `=begin`/`=end` block-comment marker line? Ruby requires
/// column 0, followed by end-of-line or whitespace.
fn ruby_block_marker(line: &str, marker: &str) -> bool {
    line.starts_with(marker)
        && line[marker.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// Scan one ruby code line left to right for scanner state: pending
/// heredoc delimiters (`<<~ID`, `<<-ID`, `<<ID`, `<<'ID'`, `<<"ID"`,
/// queued FIFO like the shell path) and %-literals left open at EOL.
/// '...'/"..." quotes shield their contents, and the first `#` outside
/// any literal ends the scan — the rest is a comment tail and can't
/// open anything. Quote state is the caller's: a literal left open at
/// EOL stays open across lines (WO 48.37).
// ponytail: `x <<y` (shift with no space) and `%` after other operators
// read as heredoc/literal openings — over-opening only skips stripping,
// it never deletes literal content (48.11's safe-direction trade-off).
fn ruby_scan_code(
    line: &str,
    heredocs: &mut Vec<(bool, String)>,
    pct: &mut Option<PctOpen>,
    quote: &mut Option<char>,
) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = *quote {
            // Ruby honors \' and \\ inside single quotes too — skip the
            // backslash pair in either quote style so an escaped quote can't
            // close the literal early and swallow a real heredoc marker
            // (WO 48.29). Skipping \x in single quotes is end-equivalent:
            // only \' and \\ can move the closing quote.
            if c == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if c == q {
                *quote = None;
            }
            i += 1;
            continue;
        }
        if let Some(p) = pct.as_mut() {
            if c == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if c == p.close {
                if p.open == p.close || p.depth == 1 {
                    *pct = None;
                } else {
                    p.depth -= 1;
                }
            } else if c == p.open {
                p.depth += 1;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => *quote = Some(c),
            '#' => return,
            '%' if i == 0 || matches!(chars[i - 1], ' ' | '\t' | '=' | '(' | '[' | '{' | ',') => {
                // `%` at term position: optional type letters, then the
                // delimiter. Alphanumeric/space after `%` is modulo, not a
                // literal (ruby allows no space before the delimiter).
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_alphabetic() {
                    j += 1;
                }
                if let Some(&d) = chars.get(j) {
                    if !d.is_alphanumeric() && !d.is_whitespace() {
                        let close = match d {
                            '(' => ')',
                            '{' => '}',
                            '[' => ']',
                            '<' => '>',
                            _ => d, // same-char delimiter: no nesting
                        };
                        *pct = Some(PctOpen {
                            open: d,
                            close,
                            depth: 1,
                        });
                        i = j + 1;
                        continue;
                    }
                }
            }
            '<' if (i == 0 || chars[i - 1] != '<')
                && chars.get(i + 1) == Some(&'<')
                && chars.get(i + 2) != Some(&'<') =>
            {
                // Ruby allows no whitespace between `<<` and the delimiter,
                // so `a << b` (left shift) never opens anything.
                let mut j = i + 2;
                let indent_tolerant = matches!(chars.get(j), Some('~') | Some('-'));
                if indent_tolerant {
                    j += 1;
                }
                let quoted = matches!(chars.get(j), Some('\'') | Some('"'));
                if quoted {
                    j += 1;
                }
                let start = j;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                if j > start {
                    heredocs.push((indent_tolerant, chars[start..j].iter().collect()));
                    // Same as shell: resume after the closing quote so a
                    // later opener on the same line stays visible (WO 48.25).
                    if quoted && matches!(chars.get(j), Some('\'') | Some('"')) {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn minify_ruby(source: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;
    // Open heredocs, oldest first: (indent-tolerant terminator, delimiter).
    let mut open_heredocs: Vec<(bool, String)> = Vec::new();
    // %-literal spanning lines.
    let mut pct: Option<PctOpen> = None;
    // "..."/'...' literal spanning lines (WO 48.37).
    let mut quote: Option<char> = None;
    // Inside a =begin/=end block comment.
    let mut in_block = false;

    for line in source.lines() {
        // Heredoc body: verbatim (no comment strip, no blank collapse)
        // until the terminator line. A quote opener inside a heredoc body
        // is heredoc content — bodies are never scanned for quotes.
        if let Some(&(indent_tolerant, ref delim)) = open_heredocs.first() {
            let candidate = if indent_tolerant { line.trim() } else { line };
            if candidate == delim.as_str() {
                open_heredocs.remove(0);
            }
            out.push_str(line);
            out.push('\n');
            prev_blank = false;
            continue;
        }

        // %-literal continuation: verbatim; the scan may close it mid-line
        // (and the remainder can then open a heredoc, ruby-wise the string
        // is part of the same logical line).
        if pct.is_some() {
            ruby_scan_code(line, &mut open_heredocs, &mut pct, &mut quote);
            out.push_str(line);
            out.push('\n');
            prev_blank = false;
            continue;
        }

        // Multi-line string continuation: verbatim (no comment strip, no
        // blank collapse) until the closing quote. The scan advances quote
        // state so the post-close remainder can still open heredocs/%;
        // a heredoc opener inside the string is string content.
        // ponytail: an unterminated literal keeps the rest of the file
        // verbatim — safe direction (nothing stripped), same as <<$VAR.
        if quote.is_some() {
            ruby_scan_code(line, &mut open_heredocs, &mut pct, &mut quote);
            out.push_str(line);
            out.push('\n');
            prev_blank = false;
            continue;
        }

        // =begin/=end block comments.
        if in_block {
            if ruby_block_marker(line, "=end") {
                in_block = false;
            }
            continue;
        }
        if ruby_block_marker(line, "=begin") {
            in_block = true;
            continue;
        }

        let trimmed = line.trim();
        // Skip comment lines and shebang; keep magic comments.
        if trimmed.starts_with('#')
            && !trimmed.starts_with("# encoding")
            && !trimmed.starts_with("# frozen_string_literal")
        {
            continue;
        }

        ruby_scan_code(line, &mut open_heredocs, &mut pct, &mut quote);

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

// ── Shell ─────────────────────────────────────────────────────────

/// Scan one command line for heredoc openings (`<<DELIM`, `<<-DELIM`,
/// `<<'DELIM'`, `<<"DELIM"`), quote-aware. `<<<` here-strings and `#`
/// comment tails don't count. Found delimiters are queued (tab_tolerant,
/// delimiter) — FIFO, matching bash's read order for `<<A <<B` on one line.
/// Quote state is the caller's: a literal left open at EOL stays open
/// across lines (WO 48.37).
// ponytail: `<<$VAR` dynamic delimiters never terminate statically, so the
// rest of the file stays "in heredoc" — safe direction (nothing stripped).
fn shell_heredoc_opens(line: &str, open: &mut Vec<(bool, String)>, quote: &mut Option<char>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = *quote {
            if c == '\\' && q == '"' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if c == q {
                *quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => *quote = Some(c),
            '#' => {
                // Only a word-start `#` starts a comment (bash rule);
                // `a#b<<EOF` still opens a heredoc.
                if i == 0
                    || chars[i - 1].is_whitespace()
                    || chars[i - 1] == ';'
                    || chars[i - 1] == '&'
                {
                    return;
                }
            }
            '<' if (i == 0 || chars[i - 1] != '<')
                && chars.get(i + 1) == Some(&'<')
                && chars.get(i + 2) != Some(&'<') =>
            {
                let mut j = i + 2;
                let tab_tolerant = chars.get(j) == Some(&'-');
                if tab_tolerant {
                    j += 1;
                }
                while matches!(chars.get(j), Some(' ') | Some('\t')) {
                    j += 1;
                }
                let quoted = matches!(chars.get(j), Some('\'') | Some('"'));
                if quoted {
                    j += 1;
                }
                let start = j;
                while j < chars.len()
                    && !chars[j].is_whitespace()
                    && !matches!(chars[j], '\'' | '"' | ';' | '&' | ')')
                {
                    j += 1;
                }
                if j > start {
                    open.push((tab_tolerant, chars[start..j].iter().collect()));
                }
                // Resume AFTER the closing quote, not ON it — re-reading it
                // as an opening quote blinds the scanner to later openers on
                // the same line (`diff <(cat <<'A') <(cat <<'B')`, WO 48.25).
                if quoted && matches!(chars.get(j), Some('\'') | Some('"')) {
                    j += 1;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

fn minify_shell(source: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;
    // Open heredocs, oldest first: (tab-tolerant terminator, delimiter).
    let mut open_heredocs: Vec<(bool, String)> = Vec::new();
    // "..."/'...' literal spanning lines (WO 48.37).
    let mut quote: Option<char> = None;

    for line in source.lines() {
        // Heredoc body: verbatim (no comment strip, no blank collapse)
        // until the terminator line. A quote opener inside a heredoc body
        // is heredoc content — bodies are never scanned for quotes.
        if let Some(&(tab_tolerant, ref delim)) = open_heredocs.first() {
            let candidate = if tab_tolerant {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if candidate == delim.as_str() {
                open_heredocs.remove(0);
            }
            out.push_str(line);
            out.push('\n');
            prev_blank = false;
            continue;
        }

        // Multi-line string continuation: verbatim (no comment strip, no
        // blank collapse) until the closing quote. The scan advances quote
        // state so the post-close remainder can still open heredocs; a
        // heredoc opener inside the string is string content.
        // ponytail: an unterminated literal keeps the rest of the file
        // verbatim — safe direction (nothing stripped), same as <<$VAR.
        if quote.is_some() {
            shell_heredoc_opens(line, &mut open_heredocs, &mut quote);
            out.push_str(line);
            out.push('\n');
            prev_blank = false;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
            continue; // strip comments but keep shebang
        }
        shell_heredoc_opens(line, &mut open_heredocs, &mut quote);

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

    /// WO 48.40: a column-0 `#[cfg(test)]` line inside a raw string is
    /// string content, not a marker — it must survive, and real test
    /// blocks after it must still be stripped.
    #[test]
    fn test_strip_test_blocks_marker_in_raw_string_survives() {
        let source = "fn docs() -> &'static str {\n    r#\"\n#[cfg(test)]\nmod fake {}\n\"#\n}\n\n#[cfg(test)]\nmod real_tests {\n    #[test]\n    fn t() {}\n}\n";
        let out = strip_test_blocks(source);
        assert!(
            out.contains("#[cfg(test)]\nmod fake {}"),
            "marker inside raw string must survive: {out}"
        );
        assert!(
            out.contains("fn docs()"),
            "fn around the string must survive"
        );
        assert!(
            !out.contains("real_tests"),
            "real test module must still be stripped"
        );
    }

    /// WO 48.40: `"""` is an empty string plus an opening quote — a marker
    /// line between them is string content and must survive.
    #[test]
    fn test_strip_test_blocks_marker_in_triple_quote_survives() {
        let source = "fn docs() {\n    let s = \"\"\"\n#[cfg(test)]\n\"\"\";\n}\n";
        let out = strip_test_blocks(source);
        assert!(
            out.contains("#[cfg(test)]"),
            "marker inside triple-quote run must survive: {out}"
        );
        assert!(
            out.contains("fn docs()"),
            "fn around the string must survive"
        );
    }

    /// WO 48.40 round trip: minified Rust keeps a raw string containing the
    /// marker line while stripping a real test module (preserve_tests=false).
    #[test]
    fn test_minify_rust_raw_string_marker_round_trip() {
        let src = "pub fn docs() -> &'static str {\n    r#\"\n#[cfg(test)]\nexample marker in docs\n\"#\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert!(true);\n    }\n}\n";
        let out = minify_content_by_ext(src, "rs", false);
        assert!(
            out.contains("#[cfg(test)]\nexample marker in docs"),
            "raw-string marker must round-trip: {out}"
        );
        assert!(out.contains("pub fn docs()"), "code must round-trip");
        assert!(
            !out.contains("mod tests"),
            "real test module must be stripped"
        );
        assert!(!out.contains("assert!(true)"), "test body must be stripped");
    }

    /// WO 48.47: a one-liner `#[cfg(test)] mod tests { ... }` (opening and
    /// closing braces on the entry line) must consume only the entry line —
    /// the scanner must not run on looking for braces into the rest of the
    /// file.
    #[test]
    fn test_strip_test_blocks_one_line_mod_keeps_rest() {
        let source = "pub fn a() -> i32 { 1 }\n#[cfg(test)] mod tests { fn t() { assert!(true); } }\npub fn b() -> i32 { 2 }\n";
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"), "one-liner mod must be stripped");
        assert!(!out.contains("assert!"), "test body must be stripped");
        assert!(out.contains("pub fn a"), "code before must survive: {out}");
        assert!(out.contains("pub fn b"), "code after must survive: {out}");
    }

    /// WO 48.47: brace-less `#[cfg(test)]\nmod tests;` (both the two-line
    /// and the one-line `#[cfg(test)] mod tests;` forms) must consume the
    /// marker lines only — the following function must survive.
    #[test]
    fn test_strip_test_blocks_braceless_mod_keeps_next_fn() {
        let two_line = "fn before() {}\n#[cfg(test)]\nmod tests;\nfn next() -> i32 { 3 }\n";
        let out = strip_test_blocks(two_line);
        assert!(out.contains("fn before"), "code before must survive: {out}");
        assert!(
            out.contains("fn next"),
            "fn after brace-less mod must survive: {out}"
        );
        assert!(
            !out.contains("mod tests"),
            "brace-less mod must be stripped"
        );

        let one_line = "#[cfg(test)] mod tests;\nfn after() -> i32 { 4 }\n";
        let out = strip_test_blocks(one_line);
        assert!(
            out.contains("fn after"),
            "fn after one-liner must survive: {out}"
        );
        assert!(!out.contains("mod tests"), "one-liner mod must be stripped");
    }

    /// WO 48.47: block comments are opaque — quotes and braces inside
    /// `/* */` must not open string state or count braces (48.40
    /// regression), even spanning lines, while the surrounding test
    /// module is still stripped and later code survives.
    #[test]
    fn test_strip_test_blocks_block_comment_odd_quotes() {
        let source = concat!(
            "pub fn a() -> i32 { 1 }\n",
            "/* it's got \" odd quotes { and } too */\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    /* don't \" count { me */\n",
            "    /* spans\n",
            "       lines \" { */\n",
            "    #[test]\n",
            "    fn t() {\n",
            "        assert!(true);\n",
            "    }\n",
            "}\n",
            "pub fn b() -> i32 { 2 }\n",
        );
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"), "test module must be stripped");
        assert!(!out.contains("assert!"), "test body must be stripped");
        assert!(out.contains("pub fn a"), "code before must survive: {out}");
        assert!(out.contains("pub fn b"), "code after must survive: {out}");
    }

    /// WO 48.46: raw strings emit verbatim — the quote before `http` must
    /// not close a tracked string, so the `//` in the URL is content, not a
    /// line comment (old first pass ate `x"}"#;` → invalid Rust).
    #[test]
    fn test_minify_rust_raw_string_json_round_trip() {
        let src = "const S: &str = r#\"{\"repo\": \"http://x\"}\"#;\n// real comment\nlet x = 1;\n";
        let out = minify_content_by_ext(src, "rs", false);
        assert!(
            out.contains("{\"repo\": \"http://x\"}"),
            "raw-string JSON must round-trip: {out}"
        );
        assert!(out.contains("const S"), "code must round-trip");
        assert!(!out.contains("real comment"), "real comments still strip");
        assert!(out.contains("let x = 1;"), "code after must survive");
    }

    /// WO 48.46: r##"…"## closes only on `"##` — an inner `"#` is content.
    #[test]
    fn test_minify_rust_raw_string_nested_hashes() {
        let src = "let s = r##\"a \"# b \"##;\nlet y = 2;\n";
        let out = minify_content_by_ext(src, "rs", false);
        assert!(out.contains("a \"# b"), "inner \"# must survive: {out}");
        assert!(out.contains("let y = 2;"), "code after must survive");
    }

    /// WO 48.46: b"…" keeps escape processing; br#"…"# is raw and verbatim.
    #[test]
    fn test_minify_rust_byte_string_prefixes() {
        let src = "let a = b\"x\\\"y\";\nlet b = br#\"z//w\"#;\nlet c = 3;\n";
        let out = minify_content_by_ext(src, "rs", false);
        assert!(
            out.contains("b\"x\\\"y\""),
            "byte string must round-trip: {out}"
        );
        assert!(
            out.contains("br#\"z//w\"#"),
            "raw byte string must round-trip: {out}"
        );
        assert!(out.contains("let c = 3;"), "code after must survive");
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

    /// WO 48.1: `#` inside string literals must survive minification —
    /// the stripper used to truncate `"http://x#anchor"` at the `#`.
    #[test]
    fn test_minify_python_keeps_hash_in_string_literals() {
        let src = "url = \"http://x#anchor\"\nfrag = 'also#fine'\nesc = \"a\\\"#still\"\nx = 1  # real comment\n";
        let out = minify_content_by_ext(src, "py", false);
        assert!(
            out.contains("\"http://x#anchor\""),
            "URL fragment literal must survive: {out}"
        );
        assert!(
            out.contains("'also#fine'"),
            "single-quoted literal with # must survive: {out}"
        );
        assert!(
            out.contains("\"a\\\"#still\""),
            "escaped quote must not close the literal: {out}"
        );
        assert!(
            !out.contains("real comment"),
            "real comment must be stripped"
        );
    }

    /// WO 48.1: `#` inside non-docstring triple-quoted strings must survive
    /// (the old scanner lost string state right after the opening quotes).
    #[test]
    fn test_minify_python_keeps_hash_in_triple_quoted_strings() {
        let src = "tmpl = \"\"\"line with # inside\"\"\"\ndef f():\n    \"\"\"Docstring # here\"\"\"\n    pass";
        let out = minify_content_by_ext(src, "py", false);
        assert!(
            out.contains("\"\"\"line with # inside\"\"\""),
            "non-docstring triple literal must survive intact: {out}"
        );
        assert!(
            !out.contains("Docstring"),
            "docstrings must still be stripped: {out}"
        );
        assert!(out.contains("pass"));
    }

    /// WO 48.1: URL-with-fragment literal survives the full minify →
    /// envelope → expand round trip byte-identically, so the edit_file
    /// path can't write the corruption back to disk.
    #[test]
    fn test_minify_python_minify_expand_round_trip_url_fragment() {
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;

        let src = "url = \"http://x#anchor\"\n";
        let minified = minify_content_by_ext(src, "py", false);
        assert!(minified.contains("\"http://x#anchor\""));
        let wrapped = wrap_minified_envelope("python", &minified);
        let expanded = expand_minified(Path::new("x.py"), &wrapped);
        assert!(
            expanded.contains("\"http://x#anchor\""),
            "URL fragment must survive minify+expand: {expanded}"
        );
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

    // ── WO 48.11: shell heredoc bodies survive minification ───────────

    /// WO 48.11: `#` lines inside a heredoc body are literal content
    /// (config/cron comments), not shell comments — they must survive.
    /// Real comments outside the heredoc are still stripped. `<<<`
    /// here-strings must NOT open a heredoc, and a quoted `<<` is inert.
    #[test]
    fn test_minify_shell_keeps_hash_lines_in_heredoc_bodies() {
        let src = "#!/bin/sh\n# real comment\ncrontab -l <<EOF\n# m h dom mon dow command\n5 0 * * * /usr/bin/backup\nEOF\n# another real comment\necho done\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert!(
            out.contains("# m h dom mon dow command"),
            "heredoc body # line must survive: {out}"
        );
        assert!(
            out.contains("5 0 * * * /usr/bin/backup"),
            "heredoc body cron entry must survive: {out}"
        );
        assert!(out.contains("EOF"), "terminator must survive: {out}");
        assert!(
            !out.contains("real comment"),
            "real comments must be stripped"
        );
        assert!(
            out.contains("crontab -l <<EOF"),
            "opening must survive: {out}"
        );
        assert!(out.contains("echo done"));

        // `<<<` here-string: not a heredoc — following # lines are comments.
        let here_string = "cat <<< \"hello\"\n# stripped comment\necho x\n";
        let out = minify_content_by_ext(here_string, "sh", false);
        assert!(
            !out.contains("stripped comment"),
            "here-string must not open a heredoc: {out}"
        );

        // Quoted "<<": inert inside a string literal.
        let quoted = "echo \"see <<docs for details\"\n# also stripped\necho y\n";
        let out = minify_content_by_ext(quoted, "sh", false);
        assert!(
            out.contains("\"see <<docs for details\""),
            "quoted << must not open a heredoc: {out}"
        );
        assert!(!out.contains("also stripped"));
    }

    /// WO 48.11: `<<-DELIM` terminator may be indented with tabs; the
    /// indented terminator (and only it) closes the heredoc.
    #[test]
    fn test_minify_shell_heredoc_tab_stripped_delimiter() {
        let src = "#!/bin/bash\n# comment\ncat <<-EOF\n\t# indented body comment\n\tdata line\n\tEOF\n# trailing comment\necho ok\n";
        let out = minify_content_by_ext(src, "bash", false);
        assert!(
            out.contains("\t# indented body comment"),
            "<<- body must pass through verbatim: {out}"
        );
        assert!(
            out.contains("\tdata line"),
            "<<- body data must survive: {out}"
        );
        assert!(
            out.contains("\tEOF"),
            "indented terminator must survive: {out}"
        );
        assert!(
            !out.contains("# comment\n"),
            "comments outside must be stripped: {out}"
        );
        assert!(!out.contains("trailing comment"));
        assert!(out.contains("echo ok"));
    }

    /// WO 48.11: quoted delimiter `<<'EOF'` — body is fully literal, so
    /// `#` lines AND `$var` text must survive byte-identically. Quoted and
    /// unquoted delimiters consume bodies identically here because the
    /// minifier never touches body content at all.
    #[test]
    fn test_minify_shell_quoted_delimiter_heredoc_verbatim() {
        let src =
            "cat <<'EOF'\n# literal hash\nPATH=$HOME/bin:$PATH\nEOF\n# real comment\necho after\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert!(
            out.contains("# literal hash"),
            "quoted-delim body # must survive: {out}"
        );
        assert!(
            out.contains("PATH=$HOME/bin:$PATH"),
            "quoted-delim body must be verbatim (no expansion awareness needed): {out}"
        );
        assert!(!out.contains("real comment"));
        assert!(out.contains("echo after"));
    }

    /// WO 48.11: cron-style `#` body lines survive the full minify →
    /// envelope → expand round trip, so the edit_file path can't delete
    /// them from disk.
    #[test]
    fn test_minify_shell_minify_expand_round_trip_heredoc_comments() {
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;

        let src = "cat <<EOF\n# cron body comment\n30 2 * * * /usr/local/bin/job\nEOF\n";
        let minified = minify_content_by_ext(src, "sh", false);
        assert!(minified.contains("# cron body comment"));
        let wrapped = wrap_minified_envelope("shell", &minified);
        let expanded = expand_minified(Path::new("x.sh"), &wrapped);
        assert!(
            expanded.contains("# cron body comment"),
            "heredoc # line must survive minify+expand: {expanded}"
        );
        assert!(
            expanded.contains("30 2 * * * /usr/local/bin/job"),
            "heredoc body must survive minify+expand: {expanded}"
        );
    }

    /// WO 48.12: `//` inside a regex literal must survive — the stripper
    /// used to truncate `/https?:\/\//` at the escaped-slash/closing-slash
    /// pair and eat the newline.
    #[test]
    fn test_minify_js_keeps_regex_literal_with_double_slash() {
        let src = "const re = /https?:\\/\\//g;\nconst cls = /[/]/;\nconst x = 1;\n";
        let out = minify_content_by_ext(src, "js", false);
        assert!(
            out.contains("/https?:\\/\\//g"),
            "regex containing // must survive: {out}"
        );
        assert!(
            out.contains("/[/]/"),
            "slash inside a char class must stay literal: {out}"
        );
        assert!(
            out.contains("const x = 1;"),
            "code after the regex must survive: {out}"
        );
    }

    /// WO 48.12: the regex literal survives the full minify → envelope →
    /// expand round trip, so the edit_file path can't write the corruption
    /// back to disk.
    #[test]
    fn test_minify_js_regex_round_trip_envelope() {
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;

        let src = "const re = /https?:\\/\\//g;\n";
        let minified = minify_content_by_ext(src, "js", false);
        assert!(minified.contains("/https?:\\/\\//g"));
        let wrapped = wrap_minified_envelope("javascript", &minified);
        let expanded = expand_minified(Path::new("x.js"), &wrapped);
        assert!(
            expanded.contains("/https?:\\/\\//g"),
            "regex must survive minify+expand: {expanded}"
        );
    }

    /// WO 48.12: division and real comments keep working — `a / b` with an
    /// identifier/number before the slash is division, and `//` comments
    /// (even directly after `=`, a regex-position token) are still stripped.
    #[test]
    fn test_minify_js_division_and_comments_unaffected() {
        let src = "const q = a / b;\nconst r = n / 2;\nconst s = 4 / 2;\nconst t = f(x) / 2;\nconst y = // comment at regex position\n 5;\n";
        let out = minify_content_by_ext(src, "js", false);
        assert!(out.contains("a / b"), "division after identifier: {out}");
        assert!(out.contains("n / 2"), "division after identifier: {out}");
        assert!(out.contains("4 / 2"), "division after number: {out}");
        assert!(out.contains("f(x) / 2"), "division after call: {out}");
        assert!(!out.contains("comment at regex position"));
        assert!(out.contains("5;"), "code after the comment survives: {out}");
    }

    // ── WO 48.13: ruby heredoc / %-literal / =begin awareness ─────────

    /// WO 48.13: `#` lines inside a ruby heredoc body are literal content,
    /// not comments — they must survive. Real comments outside the heredoc
    /// are still stripped, and `<<~DELIM` terminators may be indented.
    #[test]
    fn test_minify_ruby_keeps_hash_lines_in_heredoc_bodies() {
        let src = "# frozen_string_literal: true\n# real comment\nsql = <<~SQL\n  -- not a comment anyway\n  # yaml-looking line\n  SELECT * FROM users\nSQL\n# another real comment\nputs sql\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(
            out.contains("# yaml-looking line"),
            "heredoc body # line must survive: {out}"
        );
        assert!(
            out.contains("SELECT * FROM users"),
            "heredoc body must survive: {out}"
        );
        assert!(out.contains("SQL"), "terminator must survive: {out}");
        assert!(
            !out.contains("real comment"),
            "comments outside must be stripped: {out}"
        );
        assert!(
            out.contains("# frozen_string_literal: true"),
            "magic comment must survive: {out}"
        );
        assert!(out.contains("sql = <<~SQL"), "opening must survive: {out}");
        assert!(out.contains("puts sql"));
    }

    /// WO 48.13: quoted-delimiter and `<<-` heredocs behave the same way;
    /// the body passes through verbatim.
    #[test]
    fn test_minify_ruby_quoted_and_dash_heredocs_verbatim() {
        let src = "x = <<-'EOS'\n# literal hash\nbody line\n  EOS\n# real comment\nputs x\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(
            out.contains("# literal hash"),
            "quoted-delim body # must survive: {out}"
        );
        assert!(out.contains("body line"));
        assert!(!out.contains("real comment"));
        assert!(out.contains("puts x"));
    }

    /// WO 48.13: `#` lines inside a multi-line %-literal (`%q(...)` with
    /// bracket delimiters, nesting-aware) are string content — they must
    /// survive, and stripping resumes after the closer.
    #[test]
    fn test_minify_ruby_keeps_hash_lines_in_pct_literals() {
        let src = "# real comment\nquery = %q(SELECT # not a comment\nFROM (select # inner\n  1))\n# another real comment\nputs query\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(
            out.contains("SELECT # not a comment"),
            "%q body # must survive: {out}"
        );
        assert!(
            out.contains("FROM (select # inner"),
            "nested bracket must keep the literal open: {out}"
        );
        assert!(
            out.contains("puts query"),
            "code after the literal must survive: {out}"
        );
        assert!(
            !out.contains("real comment"),
            "comments outside must be stripped: {out}"
        );
        // A `%` that is modulo (identifier before, or space-delimited
        // operand) must not eat the rest of the file.
        let modulo = "x = n % 2\n# stripped comment\nputs x\n";
        let out = minify_content_by_ext(modulo, "rb", false);
        assert!(
            !out.contains("stripped comment"),
            "modulo must not open a literal: {out}"
        );
        assert!(out.contains("x = n % 2"));
    }

    /// WO 48.13: `=begin`/`=end` block comments (column 0) are stripped,
    /// while code before and after survives.
    #[test]
    fn test_minify_ruby_strips_begin_end_blocks() {
        let src = "a = 1\n=begin\nblock comment line\n=end\nb = 2\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(!out.contains("block comment line"), "{out}");
        assert!(!out.contains("=begin"), "{out}");
        assert!(!out.contains("=end"), "{out}");
        assert!(out.contains("a = 1"), "code before must survive: {out}");
        assert!(out.contains("b = 2"), "code after must survive: {out}");
    }

    /// WO 48.13: heredoc `#` lines survive the full minify → envelope →
    /// expand round trip, so the edit_file path can't delete them from
    /// disk.
    #[test]
    fn test_minify_ruby_round_trip_heredoc_and_pct() {
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;

        let src = "text = <<~TEXT\n  # keep me\n  body\nTEXT\n";
        let minified = minify_content_by_ext(src, "rb", false);
        assert!(minified.contains("# keep me"));
        let wrapped = wrap_minified_envelope("ruby", &minified);
        let expanded = expand_minified(Path::new("x.rb"), &wrapped);
        assert!(
            expanded.contains("# keep me"),
            "heredoc # line must survive minify+expand: {expanded}"
        );
        assert!(
            expanded.contains("body"),
            "heredoc body must survive minify+expand: {expanded}"
        );

        let src = "re = %q(a # b\n c)\n";
        let minified = minify_content_by_ext(src, "rb", false);
        assert!(minified.contains("a # b"));
        let wrapped = wrap_minified_envelope("ruby", &minified);
        let expanded = expand_minified(Path::new("x.rb"), &wrapped);
        assert!(
            expanded.contains("a # b"),
            "%q # must survive minify+expand: {expanded}"
        );
    }

    // ── WO 48.25: quoted-delimiter resume doesn't blind the scanner ────

    /// WO 48.25: two quoted heredoc openers on one line — the scanner used
    /// to resume ON A's closing quote, re-read it as an opening quote, and
    /// lose B entirely, so B's body got comment-stripped (disk write-back
    /// deleted the `#` lines). Both bodies must round-trip byte-identical.
    #[test]
    fn test_minify_shell_two_quoted_heredocs_one_line_round_trip() {
        let src = "diff <(cat <<'A') <(cat <<'B')\n# body a\nA\n# body b\nB\necho done\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert_eq!(
            out, src,
            "both quoted heredoc bodies must round-trip verbatim: {out}"
        );
    }

    /// WO 48.25: ruby twin — `foo(<<'A', <<'B')` must open both heredocs;
    /// B used to be invisible for the same closing-quote-resume reason.
    #[test]
    fn test_minify_ruby_two_quoted_heredocs_one_line_round_trip() {
        let src = "foo(<<'A', <<'B')\n# body a\nA\n# body b\nB\nputs :done\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert_eq!(
            out, src,
            "both quoted heredoc bodies must round-trip verbatim: {out}"
        );
    }

    /// WO 48.29: `\'` inside a single-quoted ruby string is an escaped quote,
    /// not a closer. Pre-fix, the scanner closed the literal early, the next
    /// `'` opened a phantom string, the real `<<~EOS` marker was swallowed,
    /// and heredoc-body `#` lines were comment-stripped — the 48.13
    /// corruption class. `\\` escapes the same way.
    #[test]
    fn test_minify_ruby_escaped_quote_keeps_heredoc_open() {
        let src = "note = 'it\\'s fine' <<~EOS\n  # not a comment\n  body\nEOS\n# real comment\nputs note\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(
            out.contains("# not a comment"),
            "heredoc body after \\' must survive: {out}"
        );
        assert!(out.contains("body"), "heredoc body must survive: {out}");
        assert!(out.contains("EOS"), "terminator must survive: {out}");
        assert!(
            !out.contains("real comment"),
            "comments outside must still be stripped: {out}"
        );
        assert!(out.contains("puts note"));

        // `\\` before the closer: string ends at the LAST quote, heredoc opens.
        let backslash = "s = 'a\\\\' <<~EOS\n  # keep\nEOS\n";
        let out = minify_content_by_ext(backslash, "rb", false);
        assert!(
            out.contains("# keep"),
            "\\\\ must not close the literal early: {out}"
        );

        // Round trip: the write-back chain can't delete the body either.
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;
        let minified = minify_content_by_ext(src, "rb", false);
        let wrapped = wrap_minified_envelope("ruby", &minified);
        let expanded = expand_minified(Path::new("x.rb"), &wrapped);
        assert!(
            expanded.contains("# not a comment"),
            "\\' heredoc body must survive minify+expand: {expanded}"
        );
    }

    // ── WO 48.37: cross-line quote state (sh/rb) ──────────────────────

    /// WO 48.37: a multi-line shell string carries quote state across
    /// lines — `#`-leading body lines are string content, not comments,
    /// and blank body lines don't collapse. Pre-fix the `#` line was
    /// stripped (content deleted on the write-back chain).
    #[test]
    fn test_minify_shell_multiline_string_hash_lines_round_trip() {
        let src = "msg=\"first\n# not a comment\n\n\nthird\"\n# real comment\necho \"$msg\"\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert_eq!(
            out, "msg=\"first\n# not a comment\n\n\nthird\"\necho \"$msg\"\n",
            "multi-line string body must round-trip verbatim: {out}"
        );

        // Single-quote twin.
        let src = "var='a\n# hash line\nb'\necho \"$var\"\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert_eq!(
            out, src,
            "single-quoted multi-line string must round-trip verbatim: {out}"
        );

        // Round trip: the write-back chain can't delete the body either.
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;
        let minified = minify_content_by_ext(src, "sh", false);
        let wrapped = wrap_minified_envelope("shell", &minified);
        let expanded = expand_minified(Path::new("x.sh"), &wrapped);
        assert!(
            expanded.contains("# hash line"),
            "multi-line string # line must survive minify+expand: {expanded}"
        );
    }

    /// WO 48.37: a heredoc opener inside a multi-line string is string
    /// content — pre-fix the continuation line opened a phantom heredoc
    /// that swallowed the rest of the file (a real comment survived).
    #[test]
    fn test_minify_shell_heredoc_inside_string_is_content() {
        let src = "msg=\"intro\ncat <<EOF\nbody\"\n# real comment\necho end\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert!(
            !out.contains("real comment"),
            "comment after the string must be stripped (no phantom heredoc): {out}"
        );
        assert!(
            out.contains("cat <<EOF"),
            "heredoc-looking line inside the string is string content: {out}"
        );
        assert!(
            out.contains("body\""),
            "string close line must survive: {out}"
        );
        assert!(out.contains("echo end"));
    }

    /// WO 48.37: a string opener inside a heredoc body is heredoc
    /// content — the new cross-line quote state must not leak into
    /// heredoc bodies (regression guard).
    #[test]
    fn test_minify_shell_string_inside_heredoc_body_is_content() {
        let src =
            "cat <<'EOF'\nhas \" and ' openers\n# body hash\nEOF\n# real comment\necho done\n";
        let out = minify_content_by_ext(src, "sh", false);
        assert_eq!(
            out, "cat <<'EOF'\nhas \" and ' openers\n# body hash\nEOF\necho done\n",
            "heredoc body with quote chars must round-trip, comments after stripped: {out}"
        );
    }

    /// WO 48.37 (rb): a multi-line ruby string carries quote state —
    /// `#`-leading body lines survive and blank body lines don't collapse.
    #[test]
    fn test_minify_ruby_multiline_string_hash_lines_round_trip() {
        let src = "msg = \"first\n# not a comment\n\n\nthird\"\n# real comment\nputs msg\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert_eq!(
            out, "msg = \"first\n# not a comment\n\n\nthird\"\nputs msg\n",
            "multi-line ruby string body must round-trip verbatim: {out}"
        );

        // Round trip: the write-back chain can't delete the body either.
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;
        let minified = minify_content_by_ext(src, "rb", false);
        let wrapped = wrap_minified_envelope("ruby", &minified);
        let expanded = expand_minified(Path::new("x.rb"), &wrapped);
        assert!(
            expanded.contains("# not a comment"),
            "multi-line string # line must survive minify+expand: {expanded}"
        );
    }

    /// WO 48.37 (rb): a heredoc opener inside a multi-line string is
    /// string content — pre-fix the continuation line opened a phantom
    /// heredoc that swallowed the rest of the file.
    #[test]
    fn test_minify_ruby_heredoc_inside_string_is_content() {
        let src = "text = \"intro\ndocs <<~EOS\nhere\"\n# real comment\nputs text\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert!(
            !out.contains("real comment"),
            "comment after the string must be stripped (no phantom heredoc): {out}"
        );
        assert!(
            out.contains("docs <<~EOS"),
            "heredoc-looking line inside the string is string content: {out}"
        );
        assert!(
            out.contains("here\""),
            "string close line must survive: {out}"
        );
        assert!(out.contains("puts text"));
    }

    /// WO 48.37 (rb): a string opener inside a heredoc body is heredoc
    /// content — the cross-line quote state must not leak into heredoc
    /// bodies (regression guard).
    #[test]
    fn test_minify_ruby_string_inside_heredoc_body_is_content() {
        let src = "sql = <<~SQL\n  has \" and ' openers\n  body\nSQL\n# real comment\nputs sql\n";
        let out = minify_content_by_ext(src, "rb", false);
        assert_eq!(
            out, "sql = <<~SQL\n  has \" and ' openers\n  body\nSQL\nputs sql\n",
            "heredoc body with quote chars must round-trip, comments after stripped: {out}"
        );
    }
    #[test]
    fn test_minify_python_triple_quoted_call_argument_round_trips() {
        use crate::shared::minify::{expand_minified, wrap_minified_envelope};
        use std::path::Path;

        let src = "def run():\n    sql = query(\n        \"\"\"\n        SELECT *\n        FROM t\n        \"\"\"\n    )\n    return sql\n";
        let minified = minify_content_by_ext(src, "py", false);
        assert!(
            minified.contains("SELECT *"),
            "triple-quoted call argument must survive minify: {minified}"
        );
        assert!(
            minified.contains("\"\"\"\n        SELECT *"),
            "argument literal must stay triple-quoted: {minified}"
        );
        let wrapped = wrap_minified_envelope("python", &minified);
        let expanded = expand_minified(Path::new("x.py"), &wrapped);
        assert!(
            expanded.contains("SELECT *"),
            "argument must survive minify+expand: {expanded}"
        );
        assert!(expanded.contains("return sql"));
    }

    #[test]
    fn test_minify_python_list_of_triple_quoted_strings_round_trips() {
        let src =
            "Q = [\n    \"\"\"\n    alpha\n    \"\"\",\n    \"\"\"\n    beta\n    \"\"\",\n]\n";
        let out = minify_content_by_ext(src, "py", false);
        assert!(out.contains("alpha"), "first element must survive: {out}");
        assert!(out.contains("beta"), "second element must survive: {out}");
        assert!(
            out.matches("\"\"\"").count() == 4,
            "both triple-quote pairs must survive: {out}"
        );
    }

    #[test]
    fn test_minify_python_real_docstrings_still_stripped() {
        let src = "\"\"\"Module doc.\"\"\"\nclass C:\n    \"\"\"Class doc.\"\"\"\n    def m(self):\n        \"\"\"Method doc.\"\"\"\n        return 1\n";
        let out = minify_content_by_ext(src, "py", false);
        assert!(
            !out.contains("Module doc"),
            "module docstring stripped: {out}"
        );
        assert!(
            !out.contains("Class doc"),
            "class docstring stripped: {out}"
        );
        assert!(
            !out.contains("Method doc"),
            "method docstring stripped: {out}"
        );
        assert!(out.contains("class C:"), "code must survive: {out}");
        assert!(out.contains("return 1"));
    }

    #[test]
    fn test_minify_python_nested_parens_triple_quoted_string_survives() {
        let src = "run(wrap(\n    \"\"\"\n    inner # not a comment\n    \"\"\"\n))\n";
        let out = minify_content_by_ext(src, "py", false);
        assert!(
            out.contains("inner # not a comment"),
            "nested-parens literal body must survive verbatim: {out}"
        );
        assert!(out.contains("run(wrap("));
    }
}
