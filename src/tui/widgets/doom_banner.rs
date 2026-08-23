//! Doom-loop warning banner.
//!
//! Renders a 3-line banner over the chat panel when the executor
//! reports a doom loop (the same tool failing the same way N turns
//! in a row). The banner shows the offending tool, the truncated
//! error, and the count, plus the three available actions (break /
//! plan / continue). The user picks one with the arrow keys + Enter;
//! the key handler in `doom_banner_keys` mutates `AppState::doom_loop`
//! to set `acknowledged = true` so the banner hides.
//!
//! The widget itself is rendering only — it doesn't reach into state
//! other than to read `doom_loop` and write nothing. That keeps the
//! render path side-effect free, matching the pattern in the rest of
//! `tui/widgets/`.

use crate::tui::app::{AppState, DoomLoopState};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Selectable action in the doom-loop banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoomLoopAction {
    /// Cancel the in-flight generation and break out of the loop.
    Break,
    /// Switch into plan mode (mirrors `/plan`).
    Plan,
    /// Dismiss the banner and let the model keep trying.
    Continue,
}

impl DoomLoopAction {
    pub const ALL: [DoomLoopAction; 3] = [
        DoomLoopAction::Break,
        DoomLoopAction::Plan,
        DoomLoopAction::Continue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DoomLoopAction::Break => "Break (cancel generation)",
            DoomLoopAction::Plan => "Plan (switch to /plan)",
            DoomLoopAction::Continue => "Continue (dismiss)",
        }
    }

    /// Right-arrow cycles forward, left-arrow cycles backward.
    pub fn next(self) -> Self {
        match self {
            DoomLoopAction::Break => DoomLoopAction::Plan,
            DoomLoopAction::Plan => DoomLoopAction::Continue,
            DoomLoopAction::Continue => DoomLoopAction::Break,
        }
    }
}

/// State held by the TUI for the doom-loop banner's selection. The
/// selection is independent of `DoomLoopState::acknowledged` —
/// the user can move the highlight before committing, and the
/// commit sets `acknowledged = true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoomLoopSelection {
    /// Index into [`DoomLoopAction::ALL`]. 0 = Break, 1 = Plan,
    /// 2 = Continue. The banner's key handler increments /
    /// decrements this on left/right.
    pub index: usize,
}

impl DoomLoopSelection {
    pub fn selected(self) -> DoomLoopAction {
        DoomLoopAction::ALL[self.index % DoomLoopAction::ALL.len()]
    }
}

/// Render the doom-loop warning banner over the full screen. The
/// banner is a centered box with 3 action buttons; the highlight is
/// driven by `selection` (held on `AppState`).
///
/// Returns `false` if the banner should not be shown (no doom-loop
/// state, or acknowledged). The caller is expected to short-circuit
/// any further overlay rendering when this returns false.
pub fn render_doom_banner(
    f: &mut Frame,
    area: Rect,
    state: &DoomLoopState,
    selection: DoomLoopSelection,
) {
    if state.acknowledged {
        return;
    }

    // Centered box, ~60% width, ~25% height (5-7 rows).
    let popup_area = centered_rect(60, 25, area);
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Doom loop detected ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Tool + count
            Constraint::Length(2), // Error preview
            Constraint::Min(1),    // Action buttons
        ])
        .split(inner);

    // Row 1: tool + count
    let header = Paragraph::new(Line::from(vec![
        Span::styled("Tool: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("{}  ", state.tool)),
        Span::styled("Count: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("{}", state.count)),
    ]));
    f.render_widget(header, chunks[0]);

    // Row 2: error preview (truncated to fit the box width).
    let preview = Paragraph::new(Line::from(vec![
        Span::styled(
            "Last error: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(state.last_error.clone()),
    ]))
    .wrap(Wrap { trim: true });
    f.render_widget(preview, chunks[1]);

    // Row 3: action buttons.
    let mut action_spans: Vec<Span> = Vec::new();
    for (i, action) in DoomLoopAction::ALL.iter().enumerate() {
        let is_selected = i == selection.index;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let label = if is_selected {
            format!("[ {} ]", action.label())
        } else {
            format!("  {}  ", action.label())
        };
        action_spans.push(Span::styled(label, style));
        if i + 1 < DoomLoopAction::ALL.len() {
            action_spans.push(Span::raw("  "));
        }
    }
    let actions = Paragraph::new(Line::from(action_spans));
    f.render_widget(actions, chunks[2]);
}

/// Convenience: render the banner iff the conditions are met (state
/// present, `count >= THRESHOLD`, not acknowledged). Returns true if
/// the banner was drawn so the caller knows to skip other overlays.
pub fn render_if_active(f: &mut Frame, area: Rect, app_state: &AppState) -> bool {
    if let Some(ref dl) = app_state.doom.doom_loop {
        if dl.count >= crate::session::executor::DoomLoopTracker::THRESHOLD && !dl.acknowledged {
            render_doom_banner(f, area, dl, app_state.doom.doom_loop_selection);
            return true;
        }
    }
    false
}

/// Center a rect of the given percentage within `area`. Used by
/// every modal-style widget in the TUI.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DoomLoopAction::next` cycles through the three options.
    #[test]
    fn doom_loop_action_next_cycles() {
        assert_eq!(DoomLoopAction::Break.next(), DoomLoopAction::Plan);
        assert_eq!(DoomLoopAction::Plan.next(), DoomLoopAction::Continue);
        assert_eq!(DoomLoopAction::Continue.next(), DoomLoopAction::Break);
    }

    /// `DoomLoopSelection::selected` returns the highlighted action.
    #[test]
    fn doom_loop_selection_selected() {
        assert_eq!(
            DoomLoopSelection { index: 0 }.selected(),
            DoomLoopAction::Break
        );
        assert_eq!(
            DoomLoopSelection { index: 1 }.selected(),
            DoomLoopAction::Plan
        );
        assert_eq!(
            DoomLoopSelection { index: 2 }.selected(),
            DoomLoopAction::Continue
        );
    }

    /// Pinning render test: the banner highlights the action at the
    /// stored selection index, NOT a hardcoded index 0. Regression pin
    /// for the WO 43.31 bug where `render_if_active` passed
    /// `DoomLoopSelection { index: 0 }` and the banner always showed
    /// "Break" highlighted while Enter ran the user's hidden choice.
    #[test]
    fn doom_banner_highlights_stored_selection_not_hardcoded_zero() {
        use crate::tui::app::DoomLoopState;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let state = DoomLoopState {
            count: 5,
            tool: "bash".into(),
            last_error: "boom".into(),
            acknowledged: false,
        };
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // index 1 → Plan highlighted. The highlighted form is "[ Plan".
        terminal
            .draw(|f| {
                render_doom_banner(f, f.area(), &state, DoomLoopSelection { index: 1 });
            })
            .unwrap();
        let buf = buffer_to_string(&terminal);
        assert!(
            buf.contains("[ Plan"),
            "selection index 1 must highlight Plan: {buf}"
        );
        assert!(
            !buf.contains("[ Break"),
            "selection index 1 must NOT highlight Break: {buf}"
        );

        // index 2 → Continue highlighted.
        terminal
            .draw(|f| {
                render_doom_banner(f, f.area(), &state, DoomLoopSelection { index: 2 });
            })
            .unwrap();
        let buf = buffer_to_string(&terminal);
        assert!(
            buf.contains("[ Continue"),
            "selection index 2 must highlight Continue: {buf}"
        );
        assert!(
            !buf.contains("[ Break"),
            "selection index 2 must NOT highlight Break: {buf}"
        );
    }

    /// Snapshot the whole terminal buffer into a flat `String`. Borrow-
    /// checker-safe: clones the buffer once instead of capturing
    /// `&terminal` in a closure over its own dimensions.
    fn buffer_to_string(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
        }
        out
    }

    /// End-to-end pin for the WO 43.31 fix: `render_if_active` must pass
    /// the stored `app_state.doom.doom_loop_selection` into the banner,
    /// not a hardcoded `DoomLoopSelection { index: 0 }`. Fails on the old
    /// code (which always highlighted Break) because the banner would
    /// show "[ Break" even though the stored selection is Plan.
    #[test]
    fn render_if_active_highlights_stored_app_state_selection() {
        use crate::shared::test_util::app_state;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = app_state();
        state.doom.doom_loop = Some(crate::tui::app::DoomLoopState {
            count: crate::session::executor::DoomLoopTracker::THRESHOLD,
            tool: "bash".into(),
            last_error: "boom".into(),
            acknowledged: false,
        });
        state.doom.doom_loop_selection = DoomLoopSelection { index: 1 };

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_if_active(f, f.area(), &state);
            })
            .unwrap();
        let buf = buffer_to_string(&terminal);
        assert!(
            buf.contains("[ Plan"),
            "render_if_active must highlight the stored Plan selection: {buf}"
        );
        assert!(
            !buf.contains("[ Break"),
            "render_if_active must NOT highlight Break when selection is Plan: {buf}"
        );
    }
}
