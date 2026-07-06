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
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
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
        "write_file" => format_write_file_diff(approval, wrap_width, reader),
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
    let (current, new_content, is_new) = old_and_new_content(approval, reader);
    if current.is_empty() && is_new {
        // New file path: show everything as additions.
        return format_new_file_diff(path, &new_content, wrap_width);
    }

    let mut out = Vec::new();
    push_header(&mut out, path, false);
    push_unified_diff(&mut out, &current, &new_content, wrap_width);
    out
}

/// Diff for `write_file`: if the file already exists, compute a
/// real unified diff between the current content and the proposed
/// new content so the user sees both deletions and additions. If the
/// file is missing or empty, fall back to the "new file" view (all
/// additions).
fn format_write_file_diff(
    approval: &PendingApproval,
    wrap_width: usize,
    reader: FileReader<'_>,
) -> Vec<String> {
    let args = &approval.args;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    if args.get("content").and_then(|v| v.as_str()).is_none() {
        return Vec::new();
    }

    let (current, new_content, is_new) = old_and_new_content(approval, reader);
    if is_new {
        return format_new_file_diff(path, &new_content, wrap_width);
    }

    // Overwrite of an existing file: show the real diff.
    let mut out = Vec::new();
    push_header(&mut out, path, false);
    push_unified_diff(&mut out, &current, &new_content, wrap_width);
    out
}

/// Line-level statistics for a file edit preview.
#[derive(Debug, Default, PartialEq)]
pub struct DiffStats {
    /// Lines that will be added.
    pub added: usize,
    /// Lines that will be removed.
    pub deleted: usize,
    /// True when the target file does not currently exist.
    pub is_new_file: bool,
}

/// Resolve the old and new file contents for an `edit_file` or
/// `write_file` approval. Returns `None` for other tool names.
pub fn diff_stats(approval: &PendingApproval, reader: FileReader<'_>) -> Option<DiffStats> {
    let (old, new, is_new) = old_and_new_content(approval, reader);
    let diff = TextDiff::from_lines(&old, &new);
    let mut added = 0usize;
    let mut deleted = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => deleted += 1,
            ChangeTag::Equal => {}
        }
    }
    Some(DiffStats {
        added,
        deleted,
        is_new_file: is_new,
    })
}

/// Resolve the old and new full file contents for an `edit_file` or
/// `write_file` approval. The boolean flag is true when the target
/// file is missing/empty, which lets callers treat the whole change
/// as a creation.
fn old_and_new_content(
    approval: &PendingApproval,
    reader: FileReader<'_>,
) -> (String, String, bool) {
    match approval.tool_name.as_str() {
        "edit_file" => {
            let args = &approval.args;
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let old_string = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_string = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let current = reader(path).unwrap_or_default();
            let new_content = if old_string.is_empty() {
                new_string.to_string()
            } else if current.contains(old_string) {
                current.replacen(old_string, new_string, 1)
            } else {
                new_string.to_string()
            };
            let is_new = current.is_empty();
            (current, new_content, is_new)
        }
        "write_file" => {
            let args = &approval.args;
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let current = reader(path).unwrap_or_default();
            let is_new = current.is_empty();
            (current, content.to_string(), is_new)
        }
        _ => (String::new(), String::new(), false),
    }
}

/// Format a new-file diff as a list of prefixed display lines.
fn format_new_file_diff(path: &str, content: &str, wrap_width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let header = format!("+++ {path} (new file)");
    out.push(header);
    out.push(String::new());
    let width = wrap_width.max(8).saturating_sub(2);
    for line in content.lines() {
        for wrapped in wrap_diff_line(line, width) {
            out.push(format!("+ {wrapped}"));
        }
    }
    out
}

/// Push the `--- filename` / `+++ filename` unified-diff header.
/// For a new file, we skip the `---` line (consistent with
/// `git diff /dev/null`).
fn push_header(out: &mut Vec<String>, path: &str, is_new: bool) {
    if !is_new {
        out.push(format!("--- {path}"));
    }
    out.push(format!("+++ {path}"));
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
            out.push(format!("{prefix} {wrapped}"));
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

/// Render a side-by-side diff as ratatui `Line`s.
///
/// `width` is the full inner dialog width in cells. Returns an empty
/// vector when `width` is below 80 or when the approval is not for
/// `edit_file`/`write_file`.
pub fn format_side_by_side_diff(
    approval: &PendingApproval,
    width: usize,
    reader: FileReader<'_>,
) -> Vec<Line<'static>> {
    if width < 80 {
        return Vec::new();
    }
    let (old, new, _) = old_and_new_content(approval, reader);
    render_side_by_side(&old, &new, width)
}

fn render_side_by_side(old: &str, new: &str, width: usize) -> Vec<Line<'static>> {
    let pane_width = (width.saturating_sub(3)) / 2;
    if pane_width < 8 {
        return Vec::new();
    }

    let diff = TextDiff::from_lines(old, new);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut old_buf: Vec<&str> = Vec::new();
    let mut new_buf: Vec<&str> = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => old_buf.push(change.value().trim_end_matches('\n')),
            ChangeTag::Insert => new_buf.push(change.value().trim_end_matches('\n')),
            ChangeTag::Equal => {
                flush_side_by_side(&mut out, &old_buf, &new_buf, pane_width);
                old_buf.clear();
                new_buf.clear();
                let text = change.value().trim_end_matches('\n');
                out.push(build_side_line(
                    text,
                    text,
                    Color::Gray,
                    Color::Gray,
                    pane_width,
                ));
            }
        }
    }
    flush_side_by_side(&mut out, &old_buf, &new_buf, pane_width);

    out
}

fn flush_side_by_side(
    out: &mut Vec<Line<'static>>,
    old_buf: &[&str],
    new_buf: &[&str],
    pane_width: usize,
) {
    if old_buf.is_empty() && new_buf.is_empty() {
        return;
    }
    let rows = old_buf.len().max(new_buf.len());
    for i in 0..rows {
        let left = old_buf.get(i).copied().unwrap_or("");
        let right = new_buf.get(i).copied().unwrap_or("");
        let (left_color, right_color) = if old_buf.is_empty() {
            (Color::DarkGray, Color::Green)
        } else if new_buf.is_empty() {
            (Color::Red, Color::DarkGray)
        } else {
            (Color::Red, Color::Green)
        };
        out.push(build_side_line(
            left,
            right,
            left_color,
            right_color,
            pane_width,
        ));
    }
}

fn build_side_line(
    left: &str,
    right: &str,
    left_color: Color,
    right_color: Color,
    pane_width: usize,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            pad_or_truncate(left, pane_width),
            Style::default().fg(left_color),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            pad_or_truncate(right, pane_width),
            Style::default().fg(right_color),
        ),
    ])
}

fn pad_or_truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let count = chars.len();
    if count > width {
        let keep = width.saturating_sub(1);
        let mut s: String = chars.into_iter().take(keep).collect();
        s.push('…');
        s
    } else {
        let pad = width.saturating_sub(count);
        format!("{text}{}", " ".repeat(pad))
    }
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

    /// `write_file` against an existing file shows a real diff with
    /// both deletions and additions, not just the new content.
    #[test]
    fn test_write_file_overwrite_shows_deletions() {
        let a = approval(
            "write_file",
            json!({
                "path": "src/existing.rs",
                "content": "fn new() {}\n"
            }),
        );
        let reader = |p: &str| {
            if p == "src/existing.rs" {
                Some("fn old() {}\n".to_string())
            } else {
                None
            }
        };
        let lines = format_edit_diff_preview(&a, 80, &reader);
        // Real diff: both --- and +++ headers.
        assert!(lines.iter().any(|l| l.starts_with("--- src/existing.rs")));
        assert!(lines.iter().any(|l| l.starts_with("+++ src/existing.rs")));
        // Deleted old content and added new content.
        assert!(lines.iter().any(|l| l.contains("- fn old()")));
        assert!(lines.iter().any(|l| l.contains("+ fn new()")));
        // Should NOT say "new file" because it's an overwrite.
        assert!(!lines.iter().any(|l| l.contains("new file")));
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
            assert!(l.chars().count() <= 32, "wrapped line too long: {l:?}");
        }
    }

    /// `diff_stats` reports added/deleted lines for an edit preview.
    #[test]
    fn test_diff_stats_for_edit_file() {
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
        let stats = diff_stats(&a, &reader).unwrap();
        assert_eq!(stats.added, 1);
        assert_eq!(stats.deleted, 1);
        assert!(!stats.is_new_file);
    }

    /// `diff_stats` for a new file reports all lines as additions.
    #[test]
    fn test_diff_stats_for_new_file() {
        let a = approval(
            "write_file",
            json!({
                "path": "src/created.rs",
                "content": "fn created() {}\n"
            }),
        );
        let reader = |_: &str| None;
        let stats = diff_stats(&a, &reader).unwrap();
        assert_eq!(stats.added, 1);
        assert_eq!(stats.deleted, 0);
        assert!(stats.is_new_file);
    }

    /// Side-by-side diff returns empty when the dialog is too narrow.
    #[test]
    fn test_side_by_side_returns_empty_when_narrow() {
        let a = approval(
            "write_file",
            json!({
                "path": "x.rs",
                "content": "fn x() {}\n"
            }),
        );
        let reader = |_: &str| None;
        let lines = format_side_by_side_diff(&a, 60, &reader);
        assert!(lines.is_empty());
    }

    /// Side-by-side diff renders two panes for a simple edit.
    #[test]
    fn test_side_by_side_renders_two_panes() {
        let a = approval(
            "edit_file",
            json!({
                "path": "x.rs",
                "old_string": "old line\n",
                "new_string": "new line\n"
            }),
        );
        let reader = |_: &str| Some("old line\n".to_string());
        let lines = format_side_by_side_diff(&a, 80, &reader);
        assert!(!lines.is_empty(), "expected side-by-side lines, got none");
        // Every line should contain the gutter between panes.
        assert!(lines
            .iter()
            .all(|l| l.spans.iter().any(|s| s.content.contains('│'))));
    }
}
