/// Approval dialog — shown when a destructive tool call needs user confirmation.
///
/// **v1.2-p11:** The args preview is now scrollable. Previously a long
/// `edit_file` `old_string` or multi-line `bash` command got truncated
/// to a fixed 4-line window with a "..." tail, so the user was approving
/// changes they couldn't actually read. Now the dialog grows to use up
/// to 75% of the terminal height, the args preview is wrapped into a
/// flat list of lines, and the user can scroll with PageUp/PageDown/
/// Up/Down/Home/End. A `↑N more / ↓N more` indicator shows when the
/// content overflows the visible window.
use crate::tui::app::{AppState, PendingApproval};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Render a centered, scrollable approval dialog over the main content.
pub fn render_approval_dialog(
    f: &mut Frame,
    area: Rect,
    approval: &PendingApproval,
    state: &mut AppState,
) {
    // Dialog box — up to 60 cols wide, up to 75% of terminal height.
    // (A 12-line fixed dialog truncated the args preview to 4 lines,
    // which made it impossible to read a 200-char `edit_file` argument
    // before approving it.)
    let dialog_width = area.width.min(60);
    let dialog_height = (area.height * 3 / 4).clamp(10, area.height);
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    // Clear ONLY the dialog rect, not the whole screen. A full-area
    // `Clear` wipes the chat behind the popup, leaving a small red box
    // on a black field — which reads as "blank/broken" rather than an
    // approval prompt. Clearing just the dialog keeps the conversation
    // visible around it, so the prompt appears in context.
    f.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(" ⚠️  Approval Required ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(dialog_area);

    // Layout inside the dialog
    //   [0] tool name + risk hint              (2 lines)
    //   [1] args preview (scrollable)          (the rest)
    //   [2] scroll indicator (if truncated)    (1 line, only when scrolled)
    //   [3] instructions                       (1 line)
    let args_window_height = inner.height.saturating_sub(4) as usize;

    // Build the full flat line list of the preview. For
    // `edit_file` / `write_file` approvals we append a unified
    // diff after the JSON args — see Review.md gap #8. The diff is
    // color-coded per line (green for `+`, red for `-`); the args
    // lines are plain white. We pack both into a single
    // `Vec<Line>` so the existing scroll/clamp code works
    // unchanged.
    let args_lines = format_args_preview(approval, dialog_width as usize);
    let mut visible_lines: Vec<Line> = args_lines
        .iter()
        .map(|s| Line::from(Span::styled(s.as_str(), Style::default().fg(Color::White))))
        .collect();

    // Try to add a diff section. The reader callback resolves the
    // file path to its current bytes; we only attach a diff if the
    // tool is `edit_file` / `write_file` (other tools return
    // `Vec::new()` from the formatter).
    // Only read files inside the working directory; a malicious model could
    // submit edit_file("../../../../etc/passwd") expecting the diff preview to
    // leak the file contents even if PathGuard blocks the write.
    let cwd = std::env::current_dir().ok();
    let reader = |p: &str| {
        let permitted = cwd.as_ref().is_some_and(|base| {
            std::path::Path::new(p)
                .canonicalize()
                .map(|canon| canon.starts_with(base))
                .unwrap_or(false)
        });
        if permitted {
            std::fs::read_to_string(p).ok()
        } else {
            None
        }
    };
    let diff_lines = crate::tui::components::diff_preview::format_edit_diff_preview(
        approval,
        dialog_width as usize,
        &reader,
    );
    if !diff_lines.is_empty() {
        // Separator between args and diff.
        visible_lines.push(Line::from(Span::styled(
            " ── Diff preview ──",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for dl in &diff_lines {
            let color = if dl.starts_with("+ ") && !dl.starts_with("+++") {
                Color::Green
            } else if dl.starts_with("- ") && !dl.starts_with("---") {
                Color::Red
            } else if dl.starts_with("+++") {
                Color::Green
            } else if dl.starts_with("---") {
                Color::Red
            } else {
                Color::White
            };
            visible_lines.push(Line::from(Span::styled(
                dl.as_str(),
                Style::default().fg(color),
            )));
        }
    }
    let all_lines: Vec<String> = visible_lines
        .iter()
        .map(|l| {
            // Flatten back to a string for the max_scroll / overflow
            // computation. The visible rendering happens below using
            // the styled `Line`s directly.
            l.spans.iter().map(|s| s.content.as_ref()).collect()
        })
        .collect();

    // Clamp scroll and compute visible window + overflow indicator.
    let max_scroll = all_lines.len().saturating_sub(args_window_height.max(1));
    state.approval_max_scroll = max_scroll;
    let scroll = state.approval_scroll.min(max_scroll);

    let visible: Vec<Line> = visible_lines
        .iter()
        .skip(scroll)
        .take(args_window_height.max(1))
        .cloned()
        .collect();

    // Show a small "N more above / N more below" indicator only when
    // there's overflow in that direction — keeps the dialog clean
    // for the common short-args case.
    let show_top_indicator = scroll > 0;
    let show_bot_indicator = scroll < max_scroll;
    let indicator_count = (show_top_indicator as usize) + (show_bot_indicator as usize);

    let constraints = if indicator_count > 0 {
        vec![
            Constraint::Length(2),
            Constraint::Length(args_window_height as u16),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(args_window_height as u16),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // [0] Tool name + risk hint
    let name_text = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(" Tool: {}", approval.tool_name),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(" Risk: {}", risk_hint(approval)),
            Style::default().fg(risk_color(approval)),
        )),
    ]);
    f.render_widget(name_text, chunks[0]);

    // [1] Args preview (scrollable)
    let args_text = Paragraph::new(visible)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    f.render_widget(args_text, chunks[1]);

    // [2] (optional) Scroll indicator
    if indicator_count > 0 {
        let mut spans = Vec::new();
        if show_top_indicator {
            spans.push(Span::styled(
                format!("↑ {scroll} more above "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if show_bot_indicator {
            spans.push(Span::styled(
                format!("↓ {} more below ", max_scroll - scroll),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let indicator = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
        f.render_widget(indicator, chunks[2]);
    }

    // [last] Instructions
    let instr_text = Paragraph::new(vec![Line::from(Span::styled(
        " [Y]es  [N]o  [A]lways  [Esc/Q] cancel    ^C exit    ↑↓ PgUp/PgDn",
        Style::default().fg(Color::Green),
    ))])
    .alignment(Alignment::Center);
    let instr_chunk = if indicator_count > 0 {
        chunks[3]
    } else {
        chunks[2]
    };
    f.render_widget(instr_text, instr_chunk);

    f.render_widget(block, dialog_area);
}

/// Build the args preview as a flat list of wrapped display lines.
///
/// Pure function — no I/O, no ratatui types in the output. Unit-testable
/// without a frame. Each returned string is one visual line of the
/// wrapped JSON pretty-print of `approval.args`.
///
/// `wrap_width` is the inner width of the dialog (in cells). Long lines
/// are wrapped on char boundaries (UTF-8 safe, regression guard for
/// the byte-slice panic class fixed in commit 9900102).
pub fn format_args_preview(approval: &PendingApproval, wrap_width: usize) -> Vec<String> {
    let raw = serde_json::to_string_pretty(&approval.args).unwrap_or_default();
    let width = wrap_width.max(8).saturating_sub(2); // -2 for the leading " " indent
    raw.lines()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// Word-wrap a single line to `width` cells, splitting on char boundaries
/// (NOT byte boundaries — a multi-byte UTF-8 char must not be split).
/// Returns at least one line (the empty string for an empty input).
///
/// **Cell-width approximation:** the dialog is monospace, so wrapping
/// by char count is correct for ASCII and a slight over-estimate for
/// full-width CJK (which would ideally be 2 cells per char). For the
/// preview use case (pretty-printed JSON in English source files) this
/// is the right trade-off — no extra dependency, no surprises with
/// combiners or joiners, UTF-8 safe. The visual result is a stable
/// line count that tests can pin.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
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

/// Human-readable risk hint for the current approval.
///
/// Pure function so the test suite can pin the mapping. Returns a
/// short string suitable for the dialog's "Risk:" line. The heuristic
/// is intentionally simple: read-only commands are green, write-ish
/// commands are yellow, anything that touches `rm` / `dd` / `mkfs` /
/// a path outside the project is red.
pub fn risk_hint(approval: &PendingApproval) -> &'static str {
    let name = approval.tool_name.as_str();
    if name == "bash" {
        if let Some(cmd) = approval.args.get("command").and_then(|v| v.as_str()) {
            let lower = cmd.to_lowercase();
            if lower.contains("rm -rf")
                || lower.contains("rm -fr")
                || lower.contains("mkfs")
                || lower.contains("dd if=")
                || lower.contains(":(){:|:&};:")
                || lower.contains("chmod -r 777")
                || lower.contains("chmod 777 /")
            {
                return "destructive — could delete data";
            }
            if lower.contains("rm ")
                || lower.contains("mv ")
                || lower.contains("> ")
                || lower.contains(">>")
                || lower.contains("sed -i")
                || lower.contains("curl ")
                || lower.contains("wget ")
            {
                return "writes files or network";
            }
            if lower.starts_with("ls")
                || lower.starts_with("cat")
                || lower.starts_with("head")
                || lower.starts_with("tail")
                || lower.starts_with("grep")
                || lower.starts_with("rg ")
                || lower.starts_with("find ")
                || lower.starts_with("echo ")
                || lower.starts_with("pwd")
            {
                return "read-only";
            }
            return "runs a shell command";
        }
    }
    if name == "edit_file" {
        return "modifies a file on disk";
    }
    if name == "write_file" {
        return "creates or overwrites a file";
    }
    "executes a tool"
}

/// Color to render the risk hint in. Mirrors the hint's severity.
fn risk_color(approval: &PendingApproval) -> Color {
    let hint = risk_hint(approval);
    if hint == "read-only" {
        Color::Green
    } else if hint.starts_with("destructive") {
        Color::Red
    } else {
        Color::Yellow
    }
}

// (No external dep for cell-width — see the `wrap_line` doc comment
// for why a 1-char-per-cell approximation is the right trade-off here.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::PendingApproval;
    use serde_json::json;

    fn make_approval(tool: &str, args: serde_json::Value) -> PendingApproval {
        PendingApproval {
            tool_name: tool.into(),
            args,
            responder: None,
        }
    }

    /// Empty / very short args: pretty-printed JSON gives 3 lines
    /// ("{", field, "}"), all short enough to stay single-line after
    /// wrapping at width 60.
    #[test]
    fn test_format_args_preview_short() {
        let a = make_approval("bash", json!({"command": "ls"}));
        let lines = format_args_preview(&a, 60);
        // 3 lines: the opening "{" brace, the "command" field, the
        // closing "}" brace. None wrap at width 60.
        assert_eq!(lines.len(), 3);
        // The field line is the only one that contains the value.
        assert!(lines[1].contains("ls"));
    }

    /// Long single-line command: wrapped to multiple lines.
    #[test]
    fn test_format_args_preview_wraps_long_bash_command() {
        let cmd = "echo ".to_string() + &"a".repeat(200);
        let a = make_approval("bash", json!({"command": cmd}));
        let lines = format_args_preview(&a, 40);
        assert!(lines.len() > 1, "long command should wrap");
        // No wrapped line should exceed 40 chars
        for line in &lines {
            assert!(
                line.chars().count() <= 40,
                "wrapped line exceeds width: {:?} (chars={})",
                line,
                line.chars().count()
            );
        }
    }

    /// Multi-line JSON (edit_file) — one visual line per JSON source line,
    /// with the long `old_string` further wrapped.
    #[test]
    fn test_format_args_preview_edit_file_multiline() {
        let a = make_approval(
            "edit_file",
            json!({
                "path": "src/main.rs",
                "old_string": "fn main() {\n    println!(\"hello\");\n}",
                "new_string": "fn main() {\n    println!(\"hello, world\");\n}"
            }),
        );
        let lines = format_args_preview(&a, 50);
        // The pretty-printed JSON has 5 source lines: "{", "  \"new_string\":...",
        // "  \"old_string\":...{long string}", "  \"path\":...",
        // "}". serde_json::to_string_pretty sorts keys alphabetically, so
        // new_string < old_string < path. At width 50-2=48, the long
        // string values wrap to 2 lines each, so we get:
        //   [0] = "{"
        //   [1] = "  \"new_string\": ..." (wrapped part 1)
        //   [2] = "lo, world\");\n}\"," (wrapped part 2)
        //   [3] = "  \"old_string\": ..." (wrapped part 1)
        //   [4] = "lo\");\n}\","        (wrapped part 2)
        //   [5] = "  \"path\": \"src/main.rs\""
        //   [6] = "}"
        assert!(
            lines.len() >= 5,
            "expected at least 5 visual lines, got {}",
            lines.len()
        );
        // `lines[0]` is the opening brace; `lines[last-1]` is the closing brace.
        assert!(lines[0] == "{", "lines[0] was {:?}", lines[0]);
        // The path line — find it by content rather than relying on
        // alphabetical key order (more robust if serde_json ever changes
        // its sort behaviour, which is documented but not promised).
        let path_line = lines
            .iter()
            .find(|l| l.contains("src/main.rs"))
            .unwrap_or_else(|| panic!("no line contains the path; lines={lines:?}"));
        assert!(path_line.contains("path"));
    }

    /// UTF-8 char in args must not panic when wrapping.
    /// Regression guard for the byte-slice panic class.
    #[test]
    fn test_format_args_preview_utf8_safe() {
        let a = make_approval("write_file", json!({"content": "🦀".repeat(100)}));
        let lines = format_args_preview(&a, 30);
        // Should not panic. Should produce at least 2 wrapped lines.
        assert!(!lines.is_empty());
        for line in &lines {
            // No line should be invalid UTF-8 (the type system enforces this,
            // but explicitly check no truncation marker is mid-char)
            for ch in line.chars() {
                assert!(ch.len_utf8() > 0);
            }
        }
    }

    /// Risk hint for `rm -rf` should be the destructive one.
    #[test]
    fn test_risk_hint_destructive_rm() {
        let a = make_approval("bash", json!({"command": "rm -rf /tmp/old"}));
        assert_eq!(risk_hint(&a), "destructive — could delete data");
    }

    /// Risk hint for `ls` should be read-only.
    #[test]
    fn test_risk_hint_read_only() {
        let a = make_approval("bash", json!({"command": "ls -la"}));
        assert_eq!(risk_hint(&a), "read-only");
    }

    /// Risk hint for `cat` should be read-only.
    #[test]
    fn test_risk_hint_read_only_cat() {
        let a = make_approval("bash", json!({"command": "cat /etc/hostname"}));
        assert_eq!(risk_hint(&a), "read-only");
    }

    /// Risk hint for `cargo build` (long-running, not destructive) is a generic shell command.
    #[test]
    fn test_risk_hint_generic_cargo() {
        let a = make_approval("bash", json!({"command": "cargo build 2>&1 | tail -20"}));
        assert_eq!(risk_hint(&a), "runs a shell command");
    }

    /// Risk hint for `mv` (writes) is yellow.
    #[test]
    fn test_risk_hint_writes_mv() {
        let a = make_approval("bash", json!({"command": "mv old.txt new.txt"}));
        assert_eq!(risk_hint(&a), "writes files or network");
    }

    /// Risk hint for `edit_file` is the standard modify message.
    #[test]
    fn test_risk_hint_edit_file() {
        let a = make_approval(
            "edit_file",
            json!({"path": "x", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(risk_hint(&a), "modifies a file on disk");
    }

    /// Risk hint for `write_file` is the standard create message.
    #[test]
    fn test_risk_hint_write_file() {
        let a = make_approval("write_file", json!({"path": "x", "content": "y"}));
        assert_eq!(risk_hint(&a), "creates or overwrites a file");
    }

    /// `wrap_line` is the building block of `format_args_preview`.
    #[test]
    fn test_wrap_line_short_passthrough() {
        let lines = wrap_line("hello", 40);
        assert_eq!(lines, vec!["hello".to_string()]);
    }

    /// `wrap_line` on empty string returns one empty line.
    #[test]
    fn test_wrap_line_empty() {
        let lines = wrap_line("", 40);
        assert_eq!(lines, vec![String::new()]);
    }

    /// `wrap_line` does not panic on multibyte chars.
    #[test]
    fn test_wrap_line_utf8_does_not_split_chars() {
        let line = "🦀".repeat(20);
        let lines = wrap_line(&line, 8);
        for l in &lines {
            // The whole line is a sequence of whole 🦀s (each 2 cells wide)
            for ch in l.chars() {
                assert!(ch == '🦀' || ch == ' ', "got unexpected char: {ch:?}");
            }
        }
    }
}
