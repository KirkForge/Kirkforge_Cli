// Expansion / pretty-printing for minified source code.
//!
//! When the model edits code inside a `<minified lang="...">` envelope, the
//! file tools strip the envelope and expand the compressed source back to
//! human-readable form before writing it to disk. This module wraps external
//! formatters (`rustfmt`, `black`, `prettier`, `gofmt`, ...) and provides a
//! language-aware fallback for cases where no formatter is installed.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Detect a `<minified lang="...">...</minified>` envelope.
///
/// Returns `(lang, inner_code)` when the content is exactly (modulo leading
/// and trailing whitespace) one minified envelope. Returns `None` for any
/// other shape so the file tools only treat explicitly tagged content as
/// minified.
pub fn extract_minified_envelope(content: &str) -> Option<(&str, &str)> {
    let trimmed = content.trim();
    if !trimmed.starts_with("<minified") {
        return None;
    }
    let tag_end = trimmed.find('>')?;
    let open_tag = &trimmed[..=tag_end];

    // Only the literal `<minified>` tag; reject `<minified-foo>`.
    if !open_tag.starts_with("<minified ") && open_tag != "<minified>" {
        return None;
    }

    let lang_start = open_tag.find(r#"lang=""#)? + 6;
    let lang_end = open_tag[lang_start..].find('"')?;
    let lang = &open_tag[lang_start..lang_start + lang_end];
    if lang.is_empty() {
        return None;
    }

    let close = trimmed.rfind("</minified>")?;
    if close + "</minified>".len() != trimmed.len() {
        return None;
    }
    if close <= tag_end + 1 {
        return None;
    }

    Some((lang, &trimmed[tag_end + 1..close]))
}

/// Return true if `content` is wrapped in a minified envelope.
pub fn has_minified_envelope(content: &str) -> bool {
    extract_minified_envelope(content).is_some()
}

/// Wrap already-minified code in a minified envelope.
/// Map a file extension to the language name used in minified envelopes.
pub fn lang_name_for_ext(ext: &str) -> String {
    match ext.to_lowercase().as_str() {
        "rs" => "rust".to_string(),
        "py" => "python".to_string(),
        "js" => "javascript".to_string(),
        "ts" => "typescript".to_string(),
        "jsx" => "jsx".to_string(),
        "tsx" => "tsx".to_string(),
        "go" => "go".to_string(),
        "c" => "c".to_string(),
        "cpp" | "hpp" | "cc" => "cpp".to_string(),
        "java" => "java".to_string(),
        "rb" => "ruby".to_string(),
        "sh" | "bash" | "zsh" => "shell".to_string(),
        "md" => "markdown".to_string(),
        "json" => "json".to_string(),
        "yaml" | "yml" => "yaml".to_string(),
        "toml" => "toml".to_string(),
        other => other.to_string(),
    }
}

pub fn wrap_minified_envelope(lang: &str, code: &str) -> String {
    if code.ends_with('\n') {
        format!("<minified lang=\"{lang}\">\n{code}</minified>")
    } else {
        format!("<minified lang=\"{lang}\">\n{code}\n</minified>")
    }
}

/// Expand minified source back to readable source.
///
/// If `minified_code` carries an envelope, the envelope is stripped and the
/// inner code is expanded according to its declared language. If no envelope
/// is present, the input is returned unchanged.
pub fn expand_minified(path: &Path, minified_code: &str) -> String {
    if let Some((lang, code)) = extract_minified_envelope(minified_code) {
        let ext = ext_for_lang(lang);
        if let Some(formatted) = try_external_formatter(code, &ext, path) {
            return formatted;
        }
        tracing::warn!(
            lang = %lang,
            path = %path.display(),
            "no external formatter available for minified expansion; using fallback"
        );
        return fallback_expand(code, &ext);
    }
    minified_code.to_string()
}

/// Expand a minified code fragment (no envelope) given a file extension.
fn ext_for_lang(lang: &str) -> String {
    let lang = lang.to_lowercase();
    match lang.as_str() {
        "rust" | "rs" => "rs".to_string(),
        "python" | "py" => "py".to_string(),
        "javascript" | "js" => "js".to_string(),
        "typescript" | "ts" => "ts".to_string(),
        "jsx" => "jsx".to_string(),
        "tsx" => "tsx".to_string(),
        "go" => "go".to_string(),
        "c" => "c".to_string(),
        "cpp" | "c++" => "cpp".to_string(),
        "java" => "java".to_string(),
        "ruby" | "rb" => "rb".to_string(),
        "shell" | "sh" | "bash" | "zsh" => "sh".to_string(),
        other => other.to_string(),
    }
}

fn try_external_formatter(code: &str, ext: &str, _path: &Path) -> Option<String> {
    match ext {
        "rs" => format_with_rustfmt(code),
        "py" => format_with_black(code).or_else(|| format_with_autopep8(code)),
        "js" | "jsx" | "ts" | "tsx" => {
            format_with_prettier(code, ext).or_else(|| format_with_deno_fmt(code, ext))
        }
        "go" => format_with_gofmt(code),
        _ => None,
    }
}

fn run_formatter_stdin(command: &str, args: &[&str], code: &str) -> Option<String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    {
        let mut stdin = child.stdin.take()?;
        stdin.write_all(code.as_bytes()).ok()?;
        // Close stdin so the formatter knows input is complete.
    }

    let output = child.wait_with_output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

fn format_with_rustfmt(code: &str) -> Option<String> {
    run_formatter_stdin("rustfmt", &["--edition", "2021", "--emit", "stdout"], code)
}

fn format_with_black(code: &str) -> Option<String> {
    run_formatter_stdin("black", &["-q", "-"], code)
}

fn format_with_autopep8(code: &str) -> Option<String> {
    run_formatter_stdin("python3", &["-m", "autopep8", "-"], code)
        .or_else(|| run_formatter_stdin("autopep8", &["-"], code))
}

fn format_with_prettier(code: &str, ext: &str) -> Option<String> {
    let parser = match ext {
        "ts" | "tsx" => "typescript",
        "jsx" => "babel",
        _ => "babel",
    };
    run_formatter_stdin(
        "prettier",
        &[
            "--stdin-filepath",
            &format!("fragment.{ext}"),
            "--parser",
            parser,
        ],
        code,
    )
}

fn format_with_deno_fmt(code: &str, ext: &str) -> Option<String> {
    run_formatter_stdin("deno", &["fmt", "--ext", ext, "-"], code)
}

fn format_with_gofmt(code: &str) -> Option<String> {
    run_formatter_stdin("gofmt", &[], code)
}

/// Best-effort fallback expansion when no external formatter is installed.
///
/// The minifier is conservative: it removes comments and collapses runs of
/// whitespace but preserves single spaces and newlines. The fallback therefore
/// only has to add whitespace around punctuation that the model is likely to
/// have collapsed when editing in minified space.
fn fallback_expand(code: &str, ext: &str) -> String {
    match ext {
        "rs" | "c" | "cpp" | "java" | "go" | "js" | "ts" | "jsx" | "tsx" => fallback_c_like(code),
        "py" => fallback_python(code),
        _ => normalize_trailing_newline(code),
    }
}

/// Normalize a single trailing newline, trimming any extra blank lines.
fn normalize_trailing_newline(code: &str) -> String {
    let trimmed = code.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// Simple C-like pretty printer. Not a substitute for rustfmt, but good
/// enough to keep code readable when no formatter is installed.
fn fallback_c_like(code: &str) -> String {
    use super::lang::prev_opens_regex;

    let mut out = String::with_capacity(code.len() * 2);
    let mut chars = code.chars().peekable();
    let mut in_string = false;
    let mut string_char = '"';
    let mut in_regex = false;
    let mut in_char_class = false;
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        // Regex literal protection (WO 48.12): emit verbatim until the
        // closing unescaped `/` — the punctuation spacing below must not
        // fire inside a regex body.
        if in_regex {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            if ch == '\n' {
                // Regex literals can't span lines — misdetected; bail.
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

        // String / char literal protection. Backticks too (WO 48.38): Go
        // raw strings and JS template literals — same delimiter set the
        // minifier uses (minify_js_like), so both halves of the round-trip
        // agree on where the literal ends. ceiling: `${}` interpolation is
        // treated as literal text — conservative-correct for round-trip
        // fidelity, code inside interpolation stays un-reindented.
        if !in_string && (ch == '"' || ch == '\'' || ch == '`') {
            in_string = true;
            string_char = ch;
            out.push(ch);
            continue;
        }
        if in_string {
            out.push(ch);
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if ch == string_char {
                in_string = false;
            }
            continue;
        }

        // Block comment protection
        if ch == '/' && chars.peek() == Some(&'*') {
            out.push(ch);
            out.push(chars.next().expect("peeked Some above"));
            while let Some(c) = chars.next() {
                out.push(c);
                if c == '*' && chars.peek() == Some(&'/') {
                    out.push(chars.next().expect("peeked Some above"));
                    break;
                }
            }
            continue;
        }

        // Line comment protection
        if ch == '/' && chars.peek() == Some(&'/') {
            out.push(ch);
            out.push(chars.next().expect("peeked Some above"));
            for c in chars.by_ref() {
                out.push(c);
                if c == '\n' {
                    break;
                }
            }
            prev_was_newline = true;
            continue;
        }

        // Regex literal open (WO 48.12): same conservative heuristic as the
        // minifier — `//` and `/*` (handled above) win over a regex open.
        if ch == '/' && prev_opens_regex(&out) {
            in_regex = true;
            in_char_class = false;
            out.push(ch);
            continue;
        }

        match ch {
            ';' => {
                out.push(';');
                if chars.peek() != Some(&'}') && chars.peek() != Some(&'\n') {
                    out.push('\n');
                    prev_was_newline = true;
                }
            }
            '{' => {
                out.push('{');
                if chars.peek() != Some(&'\n') && chars.peek() != Some(&'}') {
                    out.push('\n');
                    prev_was_newline = true;
                }
            }
            '}' => {
                if !prev_was_newline {
                    out.push('\n');
                }
                out.push('}');
                if chars.peek() != Some(&';')
                    && chars.peek() != Some(&',')
                    && chars.peek() != Some(&'\n')
                    && chars.peek() != Some(&'}')
                {
                    out.push('\n');
                    prev_was_newline = true;
                } else {
                    prev_was_newline = false;
                }
            }
            ',' => {
                out.push(',');
                if chars.peek() != Some(&' ') && chars.peek() != Some(&'\n') {
                    out.push(' ');
                }
            }
            ':' => {
                // `::` scope resolution (`std::cout`, `use std::io`): emit
                // the pair as one unit, verbatim — padding here corrupts
                // paths on disk write-back (WO 48.26).
                if chars.peek() == Some(&':') {
                    out.push(':');
                    out.push(chars.next().expect("peeked Some above"));
                    continue;
                }
                out.push(':');
                // Existing space after the colon (label form `case 1: x`)
                // normalizes to exactly one — eat-then-maybe-readd would
                // lose it for identifier-glued colons.
                if chars.peek() == Some(&' ') {
                    chars.next();
                    out.push(' ');
                    continue;
                }
                // Only add a space when the colon can't be glued to an
                // identifier: collapsed ternaries (`a?b:c`) and type keys
                // (`map[string]int`) must not gain one.
                if chars.peek().is_none_or(|&c| {
                    !(c == '\n'
                        || c == ';'
                        || c == ','
                        || c == ')'
                        || c == '}'
                        || c.is_alphanumeric()
                        || c == '_')
                }) {
                    out.push(' ');
                }
            }
            // Deliberately do NOT add spaces around operators in the
            // fallback. Distinguishing unary/binary/`!` macro calls is too
            // error-prone for a heuristic printer; external formatters are
            // the right place for operator spacing.
            '\n' => {
                if !prev_was_newline {
                    out.push('\n');
                    prev_was_newline = true;
                }
            }
            c if c.is_whitespace() => {
                if !prev_was_newline && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ => {
                out.push(ch);
                prev_was_newline = false;
            }
        }
    }

    normalize_trailing_newline(&out)
}

/// Simple Python pretty printer. Depth comes from block structure only:
/// the input's own indentation pops levels (the minifier preserves it, so
/// it is the authoritative dedent signal), block openers (`def`/`if`/`try`/
/// ... lines ending in `:`) arm the next level for collapsed input, and
/// continuation headers (else/elif/except/finally) close the previous
/// sibling block. Statement names (return/pass/...) never change depth —
/// guessing them is what swallowed code into except blocks and landed
/// `else:` one level too shallow. Multi-line string interiors pass through
/// verbatim.
fn fallback_python(code: &str) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    // Leading-whitespace width of each open block level; depth is
    // `stack.len() - 1`. Levels come from real indentation or, when the
    // model collapsed it, from block openers.
    // ponytail: widths compared relatively, output normalized to 4-space
    // units — paren-aligned continuation lines get re-normalized; upgrade
    // path is a real tokenizer.
    let mut stack: Vec<usize> = vec![0];
    let mut pending_open = false;
    let mut triple: Option<char> = None;

    for raw_line in code.lines() {
        if triple.is_some() {
            out.push_str(raw_line);
            out.push('\n');
            triple = py_triple_state(raw_line, triple);
            continue;
        }
        let stripped = raw_line.trim_start();
        if stripped.is_empty() {
            out.push('\n');
            continue;
        }
        let width = indent_cols(raw_line);
        let next_triple = py_triple_state(stripped, None);
        // End-trim only when the line doesn't continue a triple literal —
        // trailing spaces inside a string body are content.
        let line = if next_triple.is_some() {
            stripped
        } else {
            stripped.trim_end()
        };

        let word = py_first_word(line);
        let is_header = matches!(word, "else" | "elif" | "except" | "finally");
        let width_dedented = width < *stack.last().expect("initialized with one level");
        while stack.len() > 1 && *stack.last().expect("initialized with one level") > width {
            stack.pop();
        }
        if width > *stack.last().expect("initialized with one level") {
            stack.push(width);
        } else if pending_open {
            // Collapsed body: the opener's level never materialized in the
            // input's indentation — open a synthetic one at this width.
            stack.push(width);
        } else if is_header && !width_dedented && stack.len() > 1 {
            // Header on un-dedented (collapsed) input closes the previous
            // sibling block; on preserved input the width already dedented.
            stack.pop();
        }

        for _ in 1..stack.len() {
            out.push_str("    ");
        }
        out.push_str(line);
        out.push('\n');

        // `async` covers `async def`/`async for`/`async with`.
        pending_open = matches!(
            word,
            "def"
                | "if"
                | "elif"
                | "else"
                | "for"
                | "while"
                | "try"
                | "except"
                | "finally"
                | "with"
                | "class"
                | "async"
        ) && line.ends_with(':')
            && next_triple.is_none();
        triple = next_triple;
    }

    normalize_trailing_newline(&out)
}

// Column width of a line's leading indent: a tab advances to the next
// multiple of 8, other whitespace counts one column. Byte offsets rank a
// one-byte tab below 2-8 spaces even though both render wider — mixed
// tabs+spaces input then mis-nests (WO 48.50).
fn indent_cols(line: &str) -> usize {
    let mut cols = 0;
    for c in line.chars() {
        match c {
            '\t' => cols += 8 - cols % 8,
            c if c.is_whitespace() => cols += 1,
            _ => break,
        }
    }
    cols
}

// First identifier-ish word of a Python line ("" when it starts with
// punctuation).
fn py_first_word(line: &str) -> &str {
    let end = line
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(line.len());
    &line[..end]
}

// Triple-quoted-string state after one line, starting from `triple` (the
// quote char when the line begins inside a triple literal). Same idiom as
// the minifier: escape-aware, `#` comments end scanning, single-quoted
// literals never span lines (an unterminated one is invalid input — reset).
fn py_triple_state(line: &str, mut triple: Option<char>) -> Option<char> {
    let mut chars = line.chars().peekable();
    let mut single: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = triple {
            if c == '\\' {
                chars.next();
            } else if c == q && chars.peek() == Some(&q) && chars.clone().nth(1) == Some(q) {
                chars.next();
                chars.next();
                triple = None;
            }
            continue;
        }
        if let Some(q) = single {
            if c == '\\' {
                chars.next();
            } else if c == q {
                single = None;
            }
            continue;
        }
        match c {
            '#' => break,
            '"' | '\'' => {
                if chars.peek() == Some(&c) && chars.clone().nth(1) == Some(c) {
                    chars.next();
                    chars.next();
                    triple = Some(c);
                } else {
                    single = Some(c);
                }
            }
            _ => {}
        }
    }
    triple
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_envelope_basic() {
        let s = "<minified lang=\"rust\">\nfn main(){}\n</minified>";
        let (lang, code) = extract_minified_envelope(s).unwrap();
        assert_eq!(lang, "rust");
        assert_eq!(code, "\nfn main(){}\n");
    }

    #[test]
    fn extract_envelope_trims_outer_whitespace() {
        let s = "  <minified lang=\"py\">x=1</minified>  ";
        let (lang, code) = extract_minified_envelope(s).unwrap();
        assert_eq!(lang, "py");
        assert_eq!(code, "x=1");
    }

    #[test]
    fn extract_envelope_rejects_plain_text() {
        assert!(extract_minified_envelope("fn main(){}").is_none());
    }

    #[test]
    fn extract_envelope_rejects_missing_lang() {
        assert!(extract_minified_envelope("<minified>code</minified>").is_none());
    }

    #[test]
    fn extract_envelope_rejects_extra_trailing_text() {
        assert!(
            extract_minified_envelope("<minified lang=\"rust\">fn main(){}</minified> extra")
                .is_none()
        );
    }

    #[test]
    fn wrap_envelope_round_trip() {
        let wrapped = wrap_minified_envelope("rust", "fn main(){}");
        let (lang, code) = extract_minified_envelope(&wrapped).unwrap();
        assert_eq!(lang, "rust");
        // wrap_minified_envelope puts the code on its own line with a
        // trailing newline before the closing tag.
        assert_eq!(code, "\nfn main(){}\n");
    }

    #[test]
    fn wrap_envelope_preserves_existing_trailing_newline() {
        let wrapped = wrap_minified_envelope("rust", "fn main(){}\n");
        let (_, code) = extract_minified_envelope(&wrapped).unwrap();
        assert_eq!(code, "\nfn main(){}\n");
    }

    #[test]
    fn expand_no_envelope_is_unchanged() {
        let code = "fn main() {}";
        assert_eq!(expand_minified(Path::new("x.rs"), code), code);
    }

    #[test]
    fn expand_rust_envelope_invokes_rustfmt() {
        let wrapped = wrap_minified_envelope("rust", "fn main(){println!(\"hi\");}");
        let expanded = expand_minified(Path::new("x.rs"), &wrapped);
        // rustfmt should add braces on their own line and spaces.
        assert!(expanded.contains("fn main()"));
        assert!(expanded.contains("println!(\"hi\")"));
        assert!(!expanded.contains("<minified"));
    }

    #[test]
    fn fallback_c_like_adds_punctuation_whitespace() {
        let minified = "fn main(){let x=1;println!(\"{}\",x);}";
        let expanded = fallback_c_like(minified);
        assert!(expanded.contains("fn main()"));
        assert!(expanded.contains("let x=1;"));
        assert!(expanded.contains("println!(\"{}\", x)"));
    }

    #[test]
    fn fallback_python_indents_blocks() {
        let minified = "def f():\n    pass";
        let expanded = fallback_python(minified);
        assert!(expanded.contains("def f():"));
        assert!(expanded.contains("    pass"));
    }

    #[test]
    fn fallback_python_try_except_else_round_trip_byte_identical() {
        // WO 48.39: code after an except/else block was swallowed into it
        // (nothing ever dedented back out of the handler body).
        let src = "try:\n    f()\nexcept ValueError:\n    log(\"bad\")\nelse:\n    g()\nx = 1\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "py", false);
        assert_eq!(fallback_expand(&minified, "py"), src);
    }

    #[test]
    fn fallback_python_nested_blocks_pass_round_trip_byte_identical() {
        // WO 48.39: a lone `pass` used to over-dedent, then `else:`
        // pre-dedented again — the header landed one level too shallow
        // (IndentationError on disk write-back).
        let src = "def f():\n    for i in range(3):\n        if i:\n            pass\n        else:\n            continue\n    return 1\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "py", false);
        assert_eq!(fallback_expand(&minified, "py"), src);
    }

    #[test]
    fn fallback_python_multiline_string_round_trip_byte_identical() {
        // WO 48.39: multi-line string interiors were trimmed and
        // re-indented — string content corrupted on write-back.
        let src = "def f():\n    s = \"\"\"\nhello\n  world\n\"\"\"\n    return s\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "py", false);
        assert_eq!(fallback_expand(&minified, "py"), src);
    }

    #[test]
    fn fallback_python_collapsed_input_reindents_from_block_structure() {
        // The model edited in minified space and wrote flat code; block
        // openers still arm levels, headers still close sibling blocks.
        let flat = "def f():\nif a:\npass\nelse:\nreturn 1";
        assert_eq!(
            fallback_python(flat),
            "def f():\n    if a:\n        pass\n    else:\n        return 1\n"
        );
    }

    #[test]
    fn fallback_python_mixed_tabs_spaces_renests_correctly() {
        // WO 48.50: byte-width comparison ranked one tab (1 byte, 8
        // columns) below 2-8 spaces, so `else:` at 8 spaces re-nested
        // inside the 2-tab block instead of closing it (IndentationError
        // on write-back). Output is 4-space normalized; structure must
        // match the input's tab-stop column structure.
        let src = "if a:\n\tif b:\n\t\tpass\n        else:\n                x = 1\n\treturn\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "py", false);
        assert_eq!(
            fallback_expand(&minified, "py"),
            "if a:\n    if b:\n        pass\n    else:\n        x = 1\n    return\n"
        );
    }

    #[test]
    fn fallback_python_pure_tabs_control_renests_correctly() {
        // Control: pure tabs are relatively monotone in bytes too — this
        // worked pre-48.50 and must stay green.
        let src = "def f():\n\tif a:\n\t\tpass\n\treturn\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "py", false);
        assert_eq!(
            fallback_expand(&minified, "py"),
            "def f():\n    if a:\n        pass\n    return\n"
        );
    }

    #[test]
    fn fallback_c_like_colons_round_trip_byte_identical() {
        // WO 48.26: colons must survive minify -> fallback expand intact.
        let cases = [
            "std::cout << \"hi\";\n",
            "auto u = \"http://example.com/x\";\n",
            "int m = cond ? a : b;\n",
            "case 1: return x;\n",
            "default:\nbreak;\n",
        ];
        for src in cases {
            let minified = crate::shared::minify::lang::minify_content_by_ext(src, "cpp", false);
            assert_eq!(
                fallback_expand(&minified, "cpp"),
                src,
                "colon corruption for {src:?}"
            );
        }
    }

    #[test]
    fn fallback_c_like_rust_scope_resolution_round_trips() {
        let src = "use std::io;\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "rs", false);
        assert_eq!(fallback_expand(&minified, "rs"), src);
    }

    #[test]
    fn fallback_c_like_never_splits_double_colon() {
        assert!(fallback_c_like("std::cout").contains("std::cout"));
        assert!(fallback_c_like("a?b:c").contains("a?b:c"));
    }

    #[test]
    fn fallback_c_like_envelope_expansion_keeps_colons() {
        // Full pipeline (minify -> envelope -> expand). cpp has no external
        // formatter arm, so this deterministically exercises the fallback.
        // The envelope's inner code starts with a newline from the wrapper
        // tag; colons must otherwise survive byte-for-byte.
        let src = "std::cout << \"hi\";\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "cpp", false);
        let wrapped = wrap_minified_envelope("cpp", &minified);
        let expanded = expand_minified(Path::new("x.cpp"), &wrapped);
        assert_eq!(expanded, format!("\n{src}"));
        assert!(!expanded.contains(": :"));
    }

    #[test]
    fn fallback_go_raw_string_round_trips() {
        // WO 48.38: Go raw strings carry braces/semicolons/newlines that the
        // punctuation arms must not touch (minifier emits them verbatim).
        // (`:=` itself is off-limits here — the ':' arm pads it (pre-existing,
        // cosmetic, out of scope); these cases use var-decls instead.)
        let cases = [
            "var s = `a{b;c}`\n",
            "var s = `line1\nline2;{x}`\n",
            "if s == `}` {\nreturn\n}\n",
        ];
        for src in cases {
            let minified = crate::shared::minify::lang::minify_content_by_ext(src, "go", false);
            assert_eq!(
                fallback_expand(&minified, "go"),
                src,
                "raw string corruption for {src:?}"
            );
        }
    }

    #[test]
    fn fallback_js_template_literal_round_trips() {
        // WO 48.38: JS template literals — `${}` interpolation treated as
        // literal text (conservative; matches the minifier's scanner).
        let src = "const t = `a;b{c}`;\n";
        let minified = crate::shared::minify::lang::minify_content_by_ext(src, "js", false);
        assert_eq!(fallback_expand(&minified, "js"), src);
    }

    #[test]
    fn fallback_backtick_literal_does_not_leak_protection() {
        // Code after a backtick literal still gets normal re-indentation.
        let expanded = fallback_c_like("f();let s=`a;b`;g();");
        assert!(expanded.contains("`a;b`"));
        assert!(expanded.contains("f();\n"));
        assert!(expanded.contains("g();\n"));
    }
}
