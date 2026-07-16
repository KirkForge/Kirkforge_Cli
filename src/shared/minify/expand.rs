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
pub fn expand_minified_by_ext(code: &str, ext: &str) -> String {
    let path = Path::new("fragment").with_extension(ext);
    if let Some(formatted) = try_external_formatter(code, ext, &path) {
        return formatted;
    }
    fallback_expand(code, ext)
}

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
    let mut out = String::with_capacity(code.len() * 2);
    let mut chars = code.chars().peekable();
    let mut in_string = false;
    let mut string_char = '"';
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        // String / char literal protection
        if !in_string && (ch == '"' || ch == '\'') {
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
            out.push(chars.next().unwrap());
            while let Some(c) = chars.next() {
                out.push(c);
                if c == '*' && chars.peek() == Some(&'/') {
                    out.push(chars.next().unwrap());
                    break;
                }
            }
            continue;
        }

        // Line comment protection
        if ch == '/' && chars.peek() == Some(&'/') {
            out.push(ch);
            out.push(chars.next().unwrap());
            for c in chars.by_ref() {
                out.push(c);
                if c == '\n' {
                    break;
                }
            }
            prev_was_newline = true;
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
                out.push(':');
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                if chars.peek() != Some(&'\n')
                    && chars.peek() != Some(&';')
                    && chars.peek() != Some(&',')
                    && chars.peek() != Some(&')')
                    && chars.peek() != Some(&'}')
                {
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

/// Simple Python pretty printer. Adds indentation based on trailing `:` and
/// dedents on block-closing keywords.
fn fallback_python(code: &str) -> String {
    let mut out = String::with_capacity(code.len() * 2);
    let indent_unit = "    ";
    let mut indent = 0usize;

    for raw_line in code.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            out.push('\n');
            continue;
        }

        // Dedent on block-ending keywords.
        let dedent_kw = line.starts_with("else:")
            || line.starts_with("elif ")
            || line.starts_with("except")
            || line.starts_with("finally:")
            || line == "else:";
        if dedent_kw && indent > 0 {
            indent -= 1;
        }

        out.push_str(&indent_unit.repeat(indent));
        out.push_str(line);
        out.push('\n');

        // Indent after a block opener.
        if line.ends_with(':') {
            indent += 1;
        }
        // Heuristic: simple statements that terminate a one-line block.
        if indent > 0
            && (line.starts_with("return ")
                || line.starts_with("raise ")
                || line == "pass"
                || line == "break"
                || line == "continue")
        {
            indent -= 1;
        }
    }

    normalize_trailing_newline(&out)
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
}
