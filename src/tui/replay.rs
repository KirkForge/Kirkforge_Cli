//! Interactive replay TUI app — minimal single-pane stepper.
//!
//! Built on top of `crate::session::replay::ReplayStepper`. The TUI is a
//! thin shell around the pure stepper state: it owns a `ReplayStepper`,
//! renders the current turn's full detail in a scrollable `Paragraph`,
//! and translates key events into stepper calls.
//!
//! Keybindings (vim-style, matching `SessionPicker`):
//!   - `j` / `↓`  step forward
//!   - `k` / `↑`  step back
//!   - `g`        prompt for a 1-based turn number to jump to
//!   - `Enter`    expand/collapse tool-call detail (toggles a flag the
//!     render path reads — kept here as a no-op toggle for now since
//!     `render_current` already shows full detail)
//!   - `q` / `Esc` quit
//!
//! The render path always uses `ReplayStepper::render_current`, which is
//! full-fidelity (no 200/300-char truncation). The `expanded` flag is
//! retained for future use (e.g. collapsing long tool results) but is
//! not currently read by the render.

use crate::session::replay::ReplayStepper;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Input sub-mode for the replay TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    /// Normal navigation.
    Normal,
    /// User pressed `g` and is typing a 1-based turn number to jump to.
    Jump(String),
}

/// Minimal interactive replay app. Owns the stepper + UI state.
pub struct ReplayApp {
    stepper: ReplayStepper,
    /// Vertical scroll offset within the current turn's rendered text.
    /// Reset to 0 whenever the cursor moves.
    scroll: u16,
    /// Maximum scroll, recomputed each render from the paragraph line count
    /// and the visible area height.
    max_scroll: u16,
    /// Toggle from `Enter`. Currently a no-op (full detail is always shown)
    /// but kept so future collapse logic has a place to live.
    expanded: bool,
    /// Current input mode (normal vs. jump prompt).
    mode: InputMode,
    /// Set by `q` / `Esc` to signal the event loop to exit.
    should_quit: bool,
}

impl ReplayApp {
    pub fn new(stepper: ReplayStepper) -> Self {
        Self {
            stepper,
            scroll: 0,
            max_scroll: 0,
            expanded: false,
            mode: InputMode::Normal,
            should_quit: false,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Handle a key event. Returns `true` if the key was consumed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // In jump mode, all keys feed the jump input until Enter/Esc.
        if let InputMode::Jump(ref mut buf) = self.mode {
            match key.code {
                KeyCode::Esc => {
                    self.mode = InputMode::Normal;
                    return true;
                }
                KeyCode::Enter => {
                    let target = buf.trim();
                    if let Ok(n) = target.parse::<usize>() {
                        // Jump input is 1-based for user ergonomics; the
                        // stepper is 0-based.
                        if n > 0 {
                            self.stepper.jump_to(n - 1);
                        } else {
                            self.stepper.jump_to(0);
                        }
                        self.scroll = 0;
                    }
                    self.mode = InputMode::Normal;
                    return true;
                }
                KeyCode::Backspace => {
                    buf.pop();
                    return true;
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    // Cap input at a reasonable length to avoid overflow.
                    if buf.len() < 10 {
                        buf.push(c);
                    }
                    return true;
                }
                _ => {
                    // Swallow other keys while in jump mode.
                    return true;
                }
            }
        }

        // Normal mode.
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if key.modifiers == KeyModifiers::NONE {
                    if self.stepper.step_forward() {
                        self.scroll = 0;
                    }
                    return true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if key.modifiers == KeyModifiers::NONE {
                    if self.stepper.step_back() {
                        self.scroll = 0;
                    }
                    return true;
                }
            }
            KeyCode::Char('g') => {
                if key.modifiers == KeyModifiers::NONE {
                    self.mode = InputMode::Jump(String::new());
                    return true;
                }
            }
            KeyCode::Enter => {
                self.expanded = !self.expanded;
                return true;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10).min(self.max_scroll);
                return true;
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                return true;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if key.modifiers == KeyModifiers::NONE {
                    self.should_quit = true;
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    /// Render the app into the given frame area.
    pub fn render(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        // Build the body text. When the trace is empty, show a placeholder.
        let body_text = self.stepper.render_current();
        let title = if self.stepper.is_empty() {
            " Replay (empty trace) ".to_string()
        } else {
            format!(
                " Replay — turn {}/{} (id {}) ",
                self.stepper.index() + 1,
                self.stepper.len(),
                self.stepper.current().map(|r| r.turn).unwrap_or(0),
            )
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

        let inner = block.inner(area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        // Body paragraph. ratatui's `Paragraph::scroll` takes a (vertical,
        // horizontal) offset; we only use vertical.
        //
        // Count lines *before* moving `body_text` into the paragraph so we
        // can recompute `max_scroll` after the render. With
        // `Wrap { trim: false }` ratatui will soft-wrap long lines, so the
        // raw line count is a lower bound on the visible line count — good
        // enough for PageUp/PageDn (which move in chunks of 10 and j/k
        // reset scroll on every move).
        let line_count = body_text.lines().count();
        let paragraph = Paragraph::new(body_text)
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, chunks[0]);
        f.render_widget(block, area);

        // Recompute max_scroll from the rendered line count.
        let body_height = chunks[0].height as usize;
        self.max_scroll = line_count
            .saturating_sub(body_height)
            .min(u16::MAX as usize) as u16;
        if self.scroll > self.max_scroll {
            self.scroll = self.max_scroll;
        }

        // Help / status bar.
        let help = match &self.mode {
            InputMode::Normal => Line::from(vec![
                Span::styled("j/k", Style::default().fg(Color::Green)),
                Span::raw(" step  "),
                Span::styled("g", Style::default().fg(Color::Green)),
                Span::raw(" jump  "),
                Span::styled("Enter", Style::default().fg(Color::Green)),
                Span::raw(" expand  "),
                Span::styled("PgUp/PgDn", Style::default().fg(Color::Green)),
                Span::raw(" scroll  "),
                Span::styled("q", Style::default().fg(Color::Green)),
                Span::raw(" quit"),
            ]),
            InputMode::Jump(buf) => Line::from(vec![
                Span::raw("Jump to turn: "),
                Span::styled(buf.clone(), Style::default().fg(Color::Yellow)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::raw("  (Enter to confirm, Esc to cancel)"),
            ]),
        };
        let help_para = Paragraph::new(help).alignment(Alignment::Left);
        f.render_widget(help_para, chunks[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::replay::{TurnOutcome, TurnRecord};

    fn rec(turn: u32, response: &str) -> TurnRecord {
        TurnRecord {
            turn,
            timestamp: format!("2026-07-22T00:00:0{turn}Z"),
            prompt_messages: vec![],
            model_response: response.to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn j_k_step_through_records() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![
            rec(1, "a"),
            rec(2, "b"),
            rec(3, "c"),
        ]));
        assert_eq!(app.stepper.index(), 0);
        assert!(app.handle_key(key(KeyCode::Char('j'))));
        assert_eq!(app.stepper.index(), 1);
        assert!(app.handle_key(key(KeyCode::Char('j'))));
        assert_eq!(app.stepper.index(), 2);
        // Boundary — at the last record; the key is still consumed but
        // the cursor doesn't move.
        assert!(app.handle_key(key(KeyCode::Char('j'))));
        assert_eq!(app.stepper.index(), 2);
        // Walk back.
        assert!(app.handle_key(key(KeyCode::Char('k'))));
        assert_eq!(app.stepper.index(), 1);
        assert!(app.handle_key(key(KeyCode::Char('k'))));
        assert_eq!(app.stepper.index(), 0);
        // Boundary — at the first record; key consumed, cursor stays.
        assert!(app.handle_key(key(KeyCode::Char('k'))));
        assert_eq!(app.stepper.index(), 0);
    }

    #[test]
    fn arrow_keys_match_j_k() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![rec(1, "a"), rec(2, "b")]));
        assert!(app.handle_key(key(KeyCode::Down)));
        assert_eq!(app.stepper.index(), 1);
        assert!(app.handle_key(key(KeyCode::Up)));
        assert_eq!(app.stepper.index(), 0);
    }

    #[test]
    fn q_quits_and_esc_quits_in_normal_mode() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![rec(1, "a")]));
        assert!(app.handle_key(key(KeyCode::Char('q'))));
        assert!(app.should_quit());

        let mut app2 = ReplayApp::new(ReplayStepper::new(vec![rec(1, "a")]));
        assert!(app2.handle_key(key(KeyCode::Esc)));
        assert!(app2.should_quit());
    }

    #[test]
    fn g_jump_prompt_parses_one_based_index() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![
            rec(1, "a"),
            rec(2, "b"),
            rec(3, "c"),
        ]));
        // Press g to enter jump mode.
        assert!(app.handle_key(key(KeyCode::Char('g'))));
        assert_eq!(app.mode, InputMode::Jump(String::new()));
        // Type "2" — should jump to 0-based index 1.
        assert!(app.handle_key(key(KeyCode::Char('2'))));
        assert_eq!(app.mode, InputMode::Jump("2".to_string()));
        assert!(app.handle_key(key(KeyCode::Enter)));
        assert_eq!(app.mode, InputMode::Normal);
        assert_eq!(app.stepper.index(), 1);
        assert_eq!(app.stepper.current().unwrap().turn, 2);
    }

    #[test]
    fn g_jump_escape_cancels_without_moving() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![rec(1, "a"), rec(2, "b")]));
        assert!(app.handle_key(key(KeyCode::Char('g'))));
        assert!(app.handle_key(key(KeyCode::Char('9'))));
        assert!(app.handle_key(key(KeyCode::Esc)));
        assert_eq!(app.mode, InputMode::Normal);
        // Cursor unchanged.
        assert_eq!(app.stepper.index(), 0);
    }

    #[test]
    fn enter_toggles_expanded_flag() {
        let mut app = ReplayApp::new(ReplayStepper::new(vec![rec(1, "a")]));
        assert!(!app.expanded);
        assert!(app.handle_key(key(KeyCode::Enter)));
        assert!(app.expanded);
        assert!(app.handle_key(key(KeyCode::Enter)));
        assert!(!app.expanded);
    }

    #[test]
    fn empty_trace_renders_without_panic() {
        // The render path must not panic on an empty trace. We exercise it
        // via `render_current` directly (the ratatui draw path needs a
        // real terminal backend, which we can't stand up in a unit test).
        let app = ReplayApp::new(ReplayStepper::new(vec![]));
        assert_eq!(app.stepper.render_current(), "");
        assert!(app.stepper.is_empty());
    }
}
