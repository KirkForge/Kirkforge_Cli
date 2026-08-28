//! Slash-command popup — a filterable list above the input bar.
//!
//! When the user types `/`, the popup opens and shows all slash commands
//! that match the filter text. ↑/↓ selects, Enter inserts, Esc dismisses.

use crate::tui::app::SlashMenu;
use crate::tui::keys::slash_commands::complete_command;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

/// Render the slash menu popup above the input area.
///
/// The popup is a bordered box showing filtered commands with the
/// selected row highlighted. It occupies up to 8 rows above the
/// input bar, aligned to the bottom of the main content area.
/// `extras` is the `[display] extra_commands` list — gated commands
/// (WO 47.13) are hidden unless enabled.
pub fn render_slash_menu(f: &mut Frame, input_area: Rect, menu: &SlashMenu, extras: &[String]) {
    let filtered = complete_command(&menu.query, extras);
    if filtered.is_empty() {
        return;
    }
    // Clamp selection to valid range.
    let selected = menu.selected.min(filtered.len().saturating_sub(1));

    let max_rows = 8usize;
    let visible = filtered.len().min(max_rows);
    let popup_height = visible.saturating_add(2) as u16; // +2 for border

    // Place the popup above the input bar, clamped to the screen.
    let popup_top = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y: popup_top,
        width: input_area.width.min(60),
        height: popup_height,
    };

    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let style = if i == selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(format!(" {cmd} "), style)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Commands ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    let mut list_state = ListState::default();
    list_state.select(Some(selected));
    f.render_stateful_widget(list, popup_area, &mut list_state);
}
