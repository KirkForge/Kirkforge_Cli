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

    /// Number of sessions in the picker.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the picker has no sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Access the session entries (for rendering).
    pub fn entries(&self) -> &[SessionEntry] {
        &self.sessions
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
        let Some(dialog_area) = picker_dialog_area(area) else {
            // Degenerate terminal (zero height in automated tests, an
            // unsettled initial resize, or below the picker minimum).
            // Clear and show a fallback message instead of panicking
            // on layout constraints.
            f.render_widget(Clear, area);
            let msg = Paragraph::new(
                "Terminal too small for session picker.\n\
                 Please resize to at least 40×12 or press any key to start fresh.",
            )
            .alignment(Alignment::Center);
            f.render_widget(msg, area);
            return;
        };

        f.render_widget(Clear, area);

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

/// Compute the picker dialog `Rect` for a given terminal area.
///
/// Pure function mirroring `approval_dialog_area` (the safe-clamp
/// pattern). Returns `None` when the terminal is too small to hold
/// the picker (width < 40 or height < 12); the caller renders a
/// fallback message instead. This is the bounds guard for the
/// height 8-11 panic class: the prior code gated on `MIN_HEIGHT=8`
/// but clamped with `.clamp(12, h)`, so any height in 8..=11 had
/// min > max and panicked. The gate and the clamp floor now share
/// the same constant, and the clamp is expressed as
/// `.min(area.height).max(MIN_HEIGHT)` so the minimum can never
/// exceed the maximum.
fn picker_dialog_area(area: Rect) -> Option<Rect> {
    const MIN_WIDTH: u16 = 40;
    const MIN_HEIGHT: u16 = 12;
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }
    let dialog_width = area.width.clamp(40, 80);
    // Safe ordering: area.height >= MIN_HEIGHT here (guarded above),
    // so max(MIN_HEIGHT) is always <= area.height.
    let dialog_height = (area.height * 3 / 4).min(area.height).max(MIN_HEIGHT);
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let rect = Rect::new(x, y, dialog_width, dialog_height);
    debug_assert!(
        rect.x + rect.width <= area.x + area.width && rect.y + rect.height <= area.y + area.height,
        "picker_dialog_area produced a rect outside the area: rect={rect:?} area={area:?}"
    );
    Some(rect)
}

/// Human-readable byte size, mirrored from `crate::tui::commands::sessions`.
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Shorten an rfc3339 timestamp to "MM-DD HH:MM", mirrored from
/// `crate::tui::commands::sessions`.
///
/// Char-boundary guard: a non-ASCII `started_at` (session id or path
/// fragments leaking in, corrupted index lines) can have multi-byte
/// UTF-8 at the slice indices, and byte-slicing mid-char panics.
/// When any slice boundary does not land on a char boundary, degrade
/// to the full string instead of risking the slice.
fn short_ts(rfc3339: &str) -> String {
    if rfc3339.len() >= 16 && [5, 10, 11, 16].iter().all(|&i| rfc3339.is_char_boundary(i)) {
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
                path: std::path::PathBuf::from(format!("/tmp/{i}.conv.ndjson")),
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

    // ── WO 38.2: picker small-size safety ───────────────────────
    //
    // The prior code gated the fallback on MIN_HEIGHT=8 but clamped
    // dialog height with `.clamp(12, h)` — any terminal height in
    // 8..=11 had min > max and panicked. The panic was reachable at
    // startup (standalone picker) and via /resume (overlay render).

    /// Heights 8..=11 (the exact panicking range) return `None`, i.e.
    /// fall back to the "terminal too small" message instead of
    /// panicking in the clamp.
    #[test]
    fn picker_dialog_area_heights_8_to_11_return_none() {
        for h in 8u16..=11 {
            assert!(
                picker_dialog_area(Rect::new(0, 0, 80, h)).is_none(),
                "height {h} must fall back, not clamp-panic"
            );
        }
    }

    /// Too-small widths also return `None`.
    #[test]
    fn picker_dialog_area_tiny_width_returns_none() {
        assert!(picker_dialog_area(Rect::new(0, 0, 39, 40)).is_none());
        assert!(picker_dialog_area(Rect::new(0, 0, 0, 40)).is_none());
    }

    /// The minimal viable terminal (80x12) produces a full-height
    /// dialog that fits — the boundary between fallback and render.
    #[test]
    fn picker_dialog_area_min_height_12_renders() {
        let dialog =
            picker_dialog_area(Rect::new(0, 0, 80, 12)).expect("height=12 should produce a rect");
        assert_eq!(dialog.height, 12);
        assert_eq!(dialog.width, 80);
    }

    /// Fuzz-style guard (mirrors approval_dialog_area's fuzz-fit
    /// test): for every size in a small-terminal sweep, a produced
    /// rect always fits inside the area and is never degenerate.
    #[test]
    fn picker_dialog_area_rect_always_fits_inside_area() {
        for h in 0u16..=60 {
            for w in 0u16..=120 {
                let area = Rect::new(0, 0, w, h);
                if let Some(dialog) = picker_dialog_area(area) {
                    assert!(
                        dialog.x + dialog.width <= area.x + area.width,
                        "w={w} h={h}: dialog width overflow"
                    );
                    assert!(
                        dialog.y + dialog.height <= area.y + area.height,
                        "w={w} h={h}: dialog height overflow"
                    );
                    assert!(dialog.width >= 40, "w={w} h={h}: zero-ish width");
                    assert!(dialog.height >= 12, "w={w} h={h}: zero-ish height");
                }
            }
        }
    }

    /// The /resume overlay and standalone startup picker share
    /// `render`; at a height of 10 (the old clamp-panic range) the
    /// render must be None-safe: fallback message, no panic.
    #[test]
    fn render_at_height_10_is_none_safe() {
        use ratatui::{backend::TestBackend, Terminal};

        let picker = SessionPicker::new(dummy_sessions(3));
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|f| picker.render(f, f.area()))
            .expect("small-terminal render must not panic");
    }

    /// A viable size renders the full picker (title row present in
    /// the buffer) — proves the fallback split didn't break the
    /// normal path.
    #[test]
    fn render_at_viable_size_draws_picker() {
        use ratatui::{backend::TestBackend, Terminal};

        let picker = SessionPicker::new(dummy_sessions(2));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| picker.render(f, f.area()))
            .expect("viable render must succeed");
        let buffer = terminal.backend().buffer();
        let rendered: String = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            rendered.contains("Resume a recent session"),
            "picker title missing from render"
        );
    }

    /// short_ts must not panic on non-ASCII input where the byte
    /// indices would split a multi-byte char (WO 38.2 P2). Degrades
    /// to the full string.
    #[test]
    fn short_ts_non_ascii_does_not_panic() {
        // All multi-byte: every slice index lands mid-char.
        let cjk = "日期时间戳测试用例字符串继续更长一些";
        assert_eq!(short_ts(cjk), cjk);
        // Long enough in bytes, but byte 5 starts mid-emoji.
        let emoji = "🎉🎉🎉🎉 and more text here";
        assert_eq!(short_ts(emoji), emoji);
        // ASCII fast path is unchanged.
        assert_eq!(short_ts("2026-06-20T14:30:00Z"), "06-20 14:30");
    }
}
