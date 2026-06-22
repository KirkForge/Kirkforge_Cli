//! Reusable recent-session picker overlay.
//!
//! Used both as a standalone startup picker (before the main TUI event
//! loop starts) and as an in-session overlay (triggered by `/resume`
//! with no arguments). The picker is intentionally simple: a vertical
//! list with arrow-key / vim-style navigation, Enter to confirm, and
//! Esc/q to cancel.

use crate::session::session_index::SessionEntry;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

/// State for the recent-session picker overlay.
pub struct SessionPicker {
    sessions: Vec<SessionEntry>,
    selected: usize,
    confirmed: bool,
    cancelled: bool,
}

impl SessionPicker {
    pub fn new(sessions: Vec<SessionEntry>) -> Self {
        Self {
            sessions,
            selected: 0,
            confirmed: false,
            cancelled: false,
        }
    }

    pub fn next(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1).min(self.sessions.len() - 1);
        }
    }

    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn confirm(&mut self) {
        if !self.sessions.is_empty() {
            self.confirmed = true;
        }
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn selected_path(&self) -> Option<std::path::PathBuf> {
        self.sessions.get(self.selected).map(|e| e.path.clone())
    }

    /// Handle a key event while the picker is active. Returns `true` if
    /// the key was consumed by the picker.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if key.modifiers == KeyModifiers::NONE {
                    self.prev();
                    return true;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if key.modifiers == KeyModifiers::NONE {
                    self.next();
                    return true;
                }
            }
            KeyCode::Enter => {
                self.confirm();
                return true;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.cancel();
                return true;
            }
            _ => {}
        }
        false
    }

    /// Render the picker centered over the full terminal area.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);

        let dialog_width = area.width.clamp(40, 80);
        let dialog_height = (area.height * 3 / 4).clamp(12, area.height);
        let x = (area.width.saturating_sub(dialog_width)) / 2;
        let y = (area.height.saturating_sub(dialog_height)) / 2;
        let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

        let block = Block::default()
            .title(" Resume a recent session ")
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(dialog_area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .margin(1)
            .split(inner);

        let header = Row::new(vec![
            Cell::from(Span::styled(
                "ID",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "Started",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "Msgs",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "Size",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
        ]);

        let rows: Vec<Row> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let style = if i == self.selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                Row::new(vec![
                    Cell::from(e.id.clone()),
                    Cell::from(short_ts(&e.started_at)),
                    Cell::from(e.message_count.to_string()),
                    Cell::from(human_size(e.size_bytes)),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(45),
                Constraint::Percentage(25),
                Constraint::Percentage(15),
                Constraint::Percentage(15),
            ],
        )
        .header(header)
        .block(block);

        f.render_widget(table, chunks[0]);

        let help = Paragraph::new(Line::from(vec![
            Span::styled("↑/↓ or k/j", Style::default().fg(Color::Green)),
            Span::raw(" move  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" resume  "),
            Span::styled("q/Esc", Style::default().fg(Color::Green)),
            Span::raw(" start fresh"),
        ]))
        .alignment(Alignment::Center);
        f.render_widget(help, chunks[1]);
    }
}

/// Human-readable byte size, mirrored from `crate::tui::commands::sessions`.
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Shorten an rfc3339 timestamp to "MM-DD HH:MM", mirrored from
/// `crate::tui::commands::sessions`.
fn short_ts(rfc3339: &str) -> String {
    if rfc3339.len() >= 16 {
        format!("{} {}", &rfc3339[5..10], &rfc3339[11..16])
    } else {
        rfc3339.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn dummy_sessions(n: usize) -> Vec<SessionEntry> {
        (0..n)
            .map(|i| SessionEntry {
                id: format!("2026-06-{:02}-session-{:02}", i + 1, i + 1),
                path: std::path::PathBuf::from(format!("/tmp/{}.conv.ndjson", i)),
                started_at: format!("2026-06-{:02}T10:{:02}:00-07:00", i + 1, i),
                message_count: i * 5,
                size_bytes: (i as u64) * 1024,
            })
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn empty_picker_cancel_works() {
        let mut p = SessionPicker::new(vec![]);
        assert!(p.handle_key(key(KeyCode::Esc)));
        assert!(p.is_cancelled());
        assert!(!p.is_confirmed());
    }

    #[test]
    fn navigation_and_selection() {
        let mut p = SessionPicker::new(dummy_sessions(3));
        assert_eq!(p.selected, 0);
        p.handle_key(key(KeyCode::Down));
        assert_eq!(p.selected, 1);
        p.handle_key(key(KeyCode::Down));
        assert_eq!(p.selected, 2);
        p.handle_key(key(KeyCode::Down)); // clamp at bottom
        assert_eq!(p.selected, 2);
        p.handle_key(key(KeyCode::Up));
        assert_eq!(p.selected, 1);
        p.handle_key(key(KeyCode::Char('k')));
        assert_eq!(p.selected, 0);
        p.handle_key(key(KeyCode::Char('j')));
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn confirm_returns_selected_path() {
        let mut p = SessionPicker::new(dummy_sessions(3));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Enter));
        assert!(p.is_confirmed());
        assert_eq!(
            p.selected_path(),
            Some(std::path::PathBuf::from("/tmp/1.conv.ndjson"))
        );
    }

    #[test]
    fn vim_keys_need_no_modifiers() {
        let mut p = SessionPicker::new(dummy_sessions(2));
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert!(!p.handle_key(ctrl_j));
        // Selection should be unchanged.
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn human_size_and_short_ts_cover_basic_cases() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(short_ts("2026-06-20T14:30:00-07:00"), "06-20 14:30");
        assert_eq!(short_ts("nope"), "nope");
    }
}
