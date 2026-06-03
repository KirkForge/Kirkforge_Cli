/// Approval dialog — shown when a destructive tool call needs user confirmation.
use crate::tui::app::PendingApproval;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Render a centered approval dialog over the main content.
pub fn render_approval_dialog(f: &mut Frame, area: Rect, approval: &PendingApproval) {
    // Dimmed overlay
    f.render_widget(Clear, area);

    // Dialog box — centered
    let dialog_width = area.width.min(60);
    let dialog_height = 12;
    let x = (area.width - dialog_width) / 2;
    let y = (area.height - dialog_height) / 2;

    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    let block = Block::default()
        .title(" ⚠️  Approval Required ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(dialog_area);

    // Layout inside the dialog
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(inner);

    // Tool name
    let name_text = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(" Tool: {}", approval.tool_name),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ]);
    f.render_widget(name_text, chunks[0]);

    // Arguments preview
    let args_str = serde_json::to_string_pretty(&approval.args).unwrap_or_default();
    let args_text = Paragraph::new(vec![
        Line::from(Span::styled(
            " Arguments:",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            truncate_arg_preview(&args_str, dialog_width as usize - 4),
            Style::default().fg(Color::White),
        )),
    ]);
    f.render_widget(args_text, chunks[1]);

    // Instructions
    let instr_text = Paragraph::new(vec![
        Line::from(Span::styled(
            " [Y]es  [N]o  [A]lways approve for this session  [Esc] cancel",
            Style::default().fg(Color::Green),
        )),
    ])
    .alignment(Alignment::Center);
    f.render_widget(instr_text, chunks[2]);

    f.render_widget(block, dialog_area);
}

fn truncate_arg_preview(s: &str, max_width: usize) -> String {
    if s.len() <= max_width {
        s.to_string()
    } else {
        format!("{}...", &s[..max_width.saturating_sub(3)])
    }
}