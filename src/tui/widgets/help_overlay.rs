//! /help overlay — centered bordered box rendering `help_text()` output.
//!
//! Opened by `/help` (and its aliases `/h`, `/?`). The overlay sits on top
//! of the chat surface; Esc closes it, ↑/↓ scrolls. The conversation is
//! NOT polluted — the help text never enters `state.conversation.messages`
//! or the session log.

use crate::tui::app::AppState;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Render the help overlay on top of the full screen `area`.
///
/// The overlay is a centered box (80% width, 80% height) with a border, a
/// title, and the help text scrolled by `state.ui.help_overlay_scroll`.
/// A footer line shows the Esc/↑/↓ hint.
pub fn render_help_overlay(f: &mut Frame, area: Rect, state: &AppState) {
    // Clear the underlying surface so the overlay reads cleanly.
    f.render_widget(Clear, area);

    // Centered inner area: 80% of the screen, clamped so the border fits.
    let popup = centered(area, 80, 80);
    let inner = popup.intersection(area);
    if inner.width < 3 || inner.height < 3 {
        // Screen too tiny to draw the overlay — skip rather than panic.
        return;
    }

    // Split off the footer hint line from the body.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let body_area = chunks[0];
    let footer_area = chunks[1];

    let extras = crate::shared::read_shared_config(&state.services.config)
        .display
        .extra_commands
        .clone();
    let text = crate::tui::keys::slash_commands::help_text(&state.services.skill_registry, &extras);
    let lines: Vec<Line> = text.lines().map(Line::from).collect();
    let total = lines.len();

    // Body block with title + border.
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Help — Esc to close, ↑/↓ to scroll ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(block, popup);

    // Paragraph inside the border — subtract the border (2 cols/rows).
    let body_inner = Rect {
        x: body_area.x + 1,
        y: body_area.y,
        width: body_area.width.saturating_sub(2),
        height: body_area.height,
    };
    let scroll = state.ui.help_overlay_scroll;
    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(Color::White))
        .scroll((scroll as u16, 0));
    f.render_widget(paragraph, body_inner);

    // Footer: position hint + scroll indicator.
    let footer_inner = Rect {
        x: footer_area.x + 1,
        y: footer_area.y,
        width: footer_area.width.saturating_sub(2),
        height: footer_area.height,
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "Esc close · ↑↓ scroll",
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled(
            format!(" [{}/{}] ", scroll.saturating_add(1), total),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .alignment(Alignment::Right);
    f.render_widget(footer, footer_inner);
}

/// Compute a centered rect of `pct_w`% × `pct_h`% of `area`.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let popup_w = area.width.saturating_mul(pct_w) / 100;
    let popup_h = area.height.saturating_mul(pct_h) / 100;
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    Rect {
        x,
        y,
        width: popup_w,
        height: popup_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::app_state;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The overlay renders without panicking on a normal-sized terminal
    /// and shows the title + at least one line of help text.
    #[test]
    fn help_overlay_renders_title_and_body() {
        let mut state = app_state();
        state.ui.help_overlay_visible = true;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_help_overlay(f, f.area(), &state);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        // The overlay is centered, so the title is not at row 0 — scan
        // the whole buffer for the title text.
        let mut found_border = false;
        let mut found_help_text = false;
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect();
            // The title string contains "Help". Also accept the border
            // glyphs ("─") as proof the box rendered.
            if row.contains("Help") || row.contains('─') {
                found_border = true;
            }
            if row.contains("Built-in commands") || row.contains("/help") {
                found_help_text = true;
            }
        }
        assert!(found_border, "help overlay border/title should be visible");
        assert!(found_help_text, "help overlay should show help_text() body");
    }

    /// The overlay does not panic on a tiny terminal (1×1) — it skips.
    #[test]
    fn help_overlay_skips_on_tiny_terminal() {
        let mut state = app_state();
        state.ui.help_overlay_visible = true;
        let backend = TestBackend::new(1, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_help_overlay(f, f.area(), &state);
            })
            .unwrap();
    }

    /// `centered` produces a rect that fits inside the source area and is
    /// the requested percentage (rounded down).
    #[test]
    fn centered_rect_is_requested_percentage() {
        let area = Rect::new(0, 0, 100, 40);
        let inner = centered(area, 80, 50);
        assert_eq!(inner.width, 80);
        assert_eq!(inner.height, 20);
        // Centered: x = (100 - 80) / 2 = 10, y = (40 - 20) / 2 = 10.
        assert_eq!(inner.x, 10);
        assert_eq!(inner.y, 10);
    }
}
