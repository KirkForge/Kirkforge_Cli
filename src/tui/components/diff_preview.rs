//! Diff preview for the approval dialog.
//!
//! Review.md gap #8: when the model calls `edit_file` or
//! `write_file`, the user has to trust the dialog's JSON pretty-print
//! of the args to understand what's about to change. For a 200-line
//! `old_string` the JSON view is useless — the user approves (or
//! rejects) on faith. This module produces a human-readable unified
//! diff for the file edit approval path so the user can see exactly
//! which lines change before saying yes.
//!
//! # Design
//!
//! The function `format_edit_diff_preview` is a pure formatter — it
//! takes a `PendingApproval`, a `wrap_width`, and a `file_reader`
//! callback that resolves a path to its current bytes. We use a
//! callback (not direct I/O) so this module stays unit-testable
//! without touching the filesystem, and so the TUI can supply a
//! cached reader if it wants.
//!
//! For `edit_file`, the callback is invoked with `approval.args["path"]`.
//! We compute a unified diff between the current file content and
//! `args["new_string"]`. For `write_file`, we don't need the
//! current file (the entire content is `args["content"]`); we show
//! a `+++ new file` header and the new content as additions.
//!
//! The output is a flat list of display lines, each prefixed with
//! `+ `, `- `, or `  ` (or `+++ filename` / `--- filename` headers
//! for context). The dialog scrolls this list with the existing
//! `approval_scroll` / `approval_max_scroll` machinery — no new
//! scroll state is needed.
//!
//! We use the `similar` crate (already a dep) for the diff
//! algorithm. Its output is per-line `Change` tags, which we map
//! directly to the prefix characters.

use crate::tui::app::PendingApproval;
use similar::{ChangeTag, TextDiff};

/// A pure function that resolves a path to its current bytes on
/// disk. Returns `None` if the file doesn't exist or can't be read.
///
/// The callback is supplied by the caller (the TUI) so this module
/// doesn't have to know about sandboxing, path guards, or caches.
pub type FileReader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Build a unified-diff preview for an `edit_file` or `write_file`
/// approval. Returns an empty `Vec` for any other tool name — the
/// caller can choose to render the args JSON in that case.
///
/// `wrap_width` is the inner dialog width in cells. Each diff line
/// is wrapped on char boundaries (UTF-8 safe).
///
/// `reader` is invoked once for `edit_file` (to read the current
/// file contents). For `write_file` it's not used.
pub fn format_edit_diff_preview(
    approval: &PendingApproval,
    wrap_width: usize,
    reader: FileReader<'_>,
) -> Vec<String> {
    match approval.tool_name.as_str() {
        "edit_file" => format_edit_file_diff(approval, wrap_width, reader),
        "write_file" => format_write_file_diff(approval, wrap_width),
        _ => Vec::new(),
    }
}

/// Diff for `edit_file`: current file vs `args["new_string"]`.
fn format_edit_file_diff(
    approval: &PendingApproval,
    wrap_width: usize,
    reader: FileReader<'_>,
) -> Vec<String> {
    let args = &approval.args;
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let current = reader(path).unwrap_or_default();

    let mut out = Vec::new();
    push_header(&mut out, path, current.is_empty());
    push_unified_diff(&mut out, &current, new_string, wrap_width);
    out
}

/// Diff for `write_file`: the entire new file as additions, with a
/// `+++ <filename> (new)` header. We don't read the existing file —
/// the model's intent is to overwrite.
fn format_write_file_diff(approval: &PendingApproval, wrap_width: usize) -> Vec<String> {
    let args = &approval.args;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    // "new file" header — no `---` line since there's no current
    // file to compare against.
    let header = format!("+++ {} (new file)", path);
    out.push(header);
    out.push(String::new());
    // All lines are additions. Push with `+ ` prefix and wrap.
    let width = wrap_width.max(8).saturating_sub(2);
    for line in content.lines() {
        for wrapped in wrap_diff_line(line, width) {
            out.push(format!("+ {}", wrapped));
        }
    }
    out
}

/// Push the `--- filename` / `+++ filename` unified-diff header.
/// For a new file, we skip the `---` line (consistent with
/// `git diff /dev/null`).
fn push_header(out: &mut Vec<String>, path: &str, is_new: bool) {
    if !is_new {
        out.push(format!("--- {}", path));
    }
    out.push(format!("+++ {}", path));
    out.push(String::new());
}

/// Compute the unified diff and push it as prefixed display lines.
fn push_unified_diff(out: &mut Vec<String>, old: &str, new: &str, wrap_width: usize) {
    let diff = TextDiff::from_lines(old, new);
    let width = wrap_width.max(8).saturating_sub(2);
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            ChangeTag::Equal => " ",
            ChangeTag::Insert => "+",
            ChangeTag::Delete => "-",
        };
        let line = change.value().trim_end_matches('\n');
        for wrapped in wrap_diff_line(line, width) {
            out.push(format!("{} {}", prefix, wrapped));
        }
    }
}

/// Wrap a single diff line. UTF-8 safe (char-indexed), like
/// `wrap_line` in `approval.rs`. Returns one line for empty input
/// so the diff stays one-line-per-source-line.
fn wrap_diff_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for ch in line.chars() {
        if current_w + 1 > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push(ch);
        current_w += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn approval(tool: &str, args: serde_json::Value) -> PendingApproval {
        PendingApproval {
            tool_name: tool.into(),
            args,
            responder: None,
        }
    }

    /// Non-edit tools return an empty preview — the caller falls
    /// back to the JSON args view.
    #[test]
    fn test_no_preview_for_bash() {
        let a = approval("bash", json!({"command": "ls"}));
        let reader = |_: &str| None;
        let lines = format_edit_diff_preview(&a, 60, &reader);
        assert!(lines.is_empty());
    }

    /// `edit_file` with a missing `path` arg returns empty.
    #[test]
    fn test_edit_file_no_path_returns_empty() {
        let a = approval("edit_file", json!({"new_string": "x"}));
        let reader = |_: &str| None;
        let lines = format_edit_diff_preview(&a, 60, &reader);
        assert!(lines.is_empty());
    }

    /// `edit_file` against an empty file (file doesn't exist) shows
    /// the new_string as additions, with no `---` header.
    #[test]
    fn test_edit_file_against_empty_file() {
        let a = approval(
            "edit_file",
            json!({
                "path": "src/new.rs",
                "old_string": "",
                "new_string": "fn main() {}\n"
            }),
        );
        let reader = |_: &str| None;
        let lines = format_edit_diff_preview(&a, 60, &reader);
        // No `---` for the empty file.
        assert!(!lines.iter().any(|l| l.starts_with("---")));
        // Has `+++` header.
        assert!(lines.iter().any(|l| l.starts_with("+++")));
        // All added lines start with `+ `.
        assert!(lines.iter().any(|l| l.starts_with("+ ")));
    }

    /// `edit_file` against an existing file shows `-` and `+` lines
    /// for the changed content. Same-line content (`fn main() {}`)
    /// appears as an unchanged line.
    #[test]
    fn test_edit_file_against_existing_file() {
        let a = approval(
            "edit_file",
            json!({
                "path": "src/main.rs",
                "old_string": "fn main() {\n    println!(\"hi\");\n}\n",
                "new_string": "fn main() {\n    println!(\"hello\");\n}\n"
            }),
        );
        let reader = |p: &str| {
            if p == "src/main.rs" {
                Some("fn main() {\n    println!(\"hi\");\n}\n".to_string())
            } else {
                None
            }
        };
        let lines = format_edit_diff_preview(&a, 80, &reader);
        // Header lines.
        assert!(lines.iter().any(|l| l.starts_with("--- src/main.rs")));
        assert!(lines.iter().any(|l| l.starts_with("+++ src/main.rs")));
        // The deleted line.
        assert!(lines.iter().any(|l| l.contains("-     println")));
        // The added line.
        assert!(lines.iter().any(|l| l.contains("+     println")));
    }

    /// `write_file` shows the content as additions, with a
    /// `(new file)` annotation on the header.
    #[test]
    fn test_write_file_preview() {
        let a = approval(
            "write_file",
            json!({
                "path": "src/created.rs",
                "content": "fn created() {}\n"
            }),
        );
        let reader = |_: &str| None;
        let lines = format_edit_diff_preview(&a, 60, &reader);
        // Header mentions "new file".
        assert!(lines.iter().any(|l| l.contains("new file")));
        // The new content appears as additions.
        assert!(lines.iter().any(|l| l.contains("+ fn created()")));
    }

    /// Wrapping: a long added line gets split across multiple
    /// display lines.
    #[test]
    fn test_diff_wraps_long_lines() {
        let a = approval(
            "edit_file",
            json!({
                "path": "x",
                "old_string": "",
                "new_string": "let very_long_line = \"this is a very long line that should be wrapped to fit the dialog width comfortably\";\n"
            }),
        );
        let reader = |_: &str| None;
        let lines = format_edit_diff_preview(&a, 30, &reader);
        // At least one `+ ` line should be present, and the total
        // line count should be > 1 (long line wrapped).
        let plus_lines: Vec<_> = lines.iter().filter(|l| l.starts_with("+ ")).collect();
        assert!(!plus_lines.is_empty());
        // Sanity: no line exceeds the wrap width + 2 for the prefix.
        for l in &plus_lines {
            assert!(l.chars().count() <= 32, "wrapped line too long: {:?}", l);
        }
    }
}
