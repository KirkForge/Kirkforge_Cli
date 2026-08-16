//! Command palette (Ctrl+K) — a centered overlay with a search input
//! and a filtered action list.
//!
//! WO 34.1. The palette is the discovery mechanism now that the
//! persistent F1–F6 tab bar is gone. Typing fuzzy-matches the query
//! against action names; ↑↓ navigates; Enter activates (sets the
//! corresponding `ActiveTab` overlay or runs the slash command);
//! Esc closes.
//!
//! The action list is static — no allocation per frame. Filtering is
//! a simple case-insensitive substring match (the "fuzzy" contract is
//! satisfied: every query char appears in order in the matched label;
//! ponytail: a real edit-distance fuzzy matcher is YAGNI for 12
//! actions — substring match is faster and the list is short enough
//! that a user sees all of it without filtering).

use crate::tui::app::ActiveTab;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

/// A single command-palette action.
#[derive(Clone, Copy, Debug)]
pub struct PaletteAction {
    /// Display label (what the user sees + what the query matches).
    pub label: &'static str,
    /// One-line hint shown to the right of the label.
    pub hint: &'static str,
    /// What Enter does.
    pub kind: PaletteKind,
}

/// What activating a palette action does.
#[derive(Clone, Copy, Debug)]
pub enum PaletteKind {
    /// Switch to an overlay tab.
    Overlay(ActiveTab),
    /// Run a slash command (no args).
    Slash(&'static str),
    /// Enter conversation search mode (Ctrl+F equivalent).
    SearchMode,
}

/// The fixed action list, in display order.
pub const ACTIONS: &[PaletteAction] = &[
    PaletteAction {
        label: "Change model",
        hint: "F2 / Ctrl+M",
        kind: PaletteKind::Overlay(ActiveTab::Models),
    },
    PaletteAction {
        label: "Open sessions",
        hint: "F6 / Ctrl+S",
        kind: PaletteKind::Overlay(ActiveTab::Sessions),
    },
    PaletteAction {
        label: "View jobs",
        hint: "F4 / Ctrl+J",
        kind: PaletteKind::Overlay(ActiveTab::Jobs),
    },
    PaletteAction {
        label: "Open settings",
        hint: "F5 / Ctrl+,",
        kind: PaletteKind::Overlay(ActiveTab::Settings),
    },
    PaletteAction {
        label: "Open plugins",
        hint: "F3 / Ctrl+P",
        kind: PaletteKind::Overlay(ActiveTab::Plugins),
    },
    PaletteAction {
        label: "Search conversation",
        hint: "Ctrl+F",
        kind: PaletteKind::SearchMode,
    },
    PaletteAction {
        label: "Compact conversation",
        hint: "/compact",
        kind: PaletteKind::Slash("/compact"),
    },
    PaletteAction {
        label: "Show help",
        hint: "/help",
        kind: PaletteKind::Slash("/help"),
    },
    PaletteAction {
        label: "Run tests",
        hint: "/test",
        kind: PaletteKind::Slash("/test"),
    },
    PaletteAction {
        label: "Commit changes",
        hint: "/commit",
        kind: PaletteKind::Slash("/commit"),
    },
    PaletteAction {
        label: "Undo last edit",
        hint: "/undo",
        kind: PaletteKind::Slash("/undo"),
    },
    PaletteAction {
        label: "Clear conversation",
        hint: "/clear",
        kind: PaletteKind::Slash("/clear"),
    },
];

/// Case-insensitive substring match: every char of `query` appears in
/// `label` in order (a lightweight fuzzy match). Empty query matches all.
pub fn matches(query: &str, label: &str) -> bool {
    let q = query.to_lowercase();
    if q.is_empty() {
        return true;
    }
    let l = label.to_lowercase();
    let mut qi = q.chars().peekable();
    for c in l.chars() {
        if qi.peek() == Some(&c) {
            qi.next();
        }
    }
    qi.peek().is_none()
}

/// Return the filtered action indices for the current query.
pub fn filtered_indices(query: &str) -> Vec<usize> {
    ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, a)| matches(query, a.label))
        .map(|(i, _)| i)
        .collect()
}

/// Render the command palette as a centered overlay on top of the
/// current frame. `query` is the search text, `selected` is the
/// highlighted row index into the *filtered* list.
pub fn render_command_palette(f: &mut Frame, area: Rect, query: &str, selected: usize) {
    // Centered popup: 60% width, up to 16 rows tall, vertically centered.
    let popup = centered_rect(area, 60, 16);
    // Clear the area underneath so the chat doesn't bleed through.
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Command Palette (Ctrl+K) ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Split inner into the search input (top) + the filtered list (below).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Search input line.
    let prompt = format!("> {query}");
    f.render_widget(
        Paragraph::new(prompt.as_str()).style(Style::default().fg(Color::Yellow)),
        chunks[0],
    );

    // Filtered list.
    let indices = filtered_indices(query);
    let items: Vec<ListItem> = indices
        .iter()
        .map(|&i| {
            let a = &ACTIONS[i];
            ListItem::new(Line::from(vec![
                Span::raw(a.label),
                Span::styled(
                    format!("  {}", a.hint),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let mut list_state = ListState::default();
    if !indices.is_empty() {
        list_state.select(Some(selected.min(indices.len() - 1)));
    }
    f.render_stateful_widget(list, chunks[1], &mut list_state);
}

/// Helper: a centered rect of `percent_x` width and up to `max_h` rows.
fn centered_rect(area: Rect, percent_x: u16, max_h: u16) -> Rect {
    let pop_h = max_h.min(area.height);
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(pop_h)) / 2),
            Constraint::Length(pop_h),
            Constraint::Min(0),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_all() {
        for a in ACTIONS {
            assert!(matches("", a.label), "empty query should match {a:?}");
        }
    }

    #[test]
    fn substring_match_case_insensitive() {
        assert!(matches("mod", "Change model"));
        assert!(matches("MOD", "Change model"));
        assert!(matches("cm", "Compact conversation"));
    }

    #[test]
    fn non_match_returns_false() {
        assert!(!matches("zzz", "Change model"));
    }

    #[test]
    fn filtered_indices_empty_query_returns_all() {
        let idx = filtered_indices("");
        assert_eq!(idx.len(), ACTIONS.len());
    }

    #[test]
    fn filtered_indices_substring_filters() {
        // "comp" matches "Compact conversation" only.
        let idx = filtered_indices("comp");
        assert_eq!(idx.len(), 1);
        assert_eq!(ACTIONS[idx[0]].label, "Compact conversation");
    }

    #[test]
    fn fuzzy_order_match() {
        // "cs" → 'c' then 's' in order: "Change model" has 'c' at 0, no 's'.
        assert!(!matches("cs", "Change model"));
        // "cs" → "Clear conversation" has 'C'(c) then 's' (in "conversation").
        assert!(matches("cs", "Clear conversation"));
    }

    #[test]
    fn actions_have_no_duplicate_labels() {
        let mut labels: Vec<&str> = ACTIONS.iter().map(|a| a.label).collect();
        labels.sort();
        let before = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), before, "duplicate action labels");
    }

    #[test]
    fn overlay_actions_cover_non_chat_tabs() {
        // Every overlay tab except Chat must have a palette action.
        for tab in ActiveTab::OVERLAYS {
            if tab == ActiveTab::Chat {
                continue;
            }
            let found = ACTIONS.iter().any(|a| match a.kind {
                PaletteKind::Overlay(t) => t == tab,
                _ => false,
            });
            assert!(found, "overlay {tab:?} has no palette action");
        }
    }

    #[test]
    fn search_mode_action_present() {
        let found = ACTIONS
            .iter()
            .any(|a| matches!(a.kind, PaletteKind::SearchMode));
        assert!(found, "a SearchMode action must exist");
    }
}
