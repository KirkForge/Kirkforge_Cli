//! File completer popup — a directory browser above the input bar.
//!
//! When the user types `@`, this popup shows files/dirs in the current
//! directory. ↓/Enter descend into directories, Backspace goes up to
//! the parent, Esc dismisses.

use crate::tui::app::FileCompleter;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

/// Render the file completer popup above the input area.
///
/// Shows the current directory header and up to 8 entries with the
/// selected row highlighted.  In directory-pick mode (`pick_directory`),
/// the title reads "Open Directory" and non-directory entries are dimmed.
pub fn render_file_completer(f: &mut Frame, input_area: Rect, completer: &FileCompleter) {
    if completer.entries.is_empty() {
        return;
    }

    let max_rows = 8usize;
    let visible = completer.entries.len().min(max_rows);
    let popup_height = visible.saturating_add(3) as u16; // +3: border + header

    // Place the popup above the input bar, clamped to the screen.
    let popup_top = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y: popup_top,
        width: input_area.width.min(60),
        height: popup_height,
    };

    let selected = completer
        .selected
        .min(completer.entries.len().saturating_sub(1));
    let dir_display = completer.dir.display().to_string();
    let title = if completer.pick_directory {
        " Open Directory "
    } else {
        " Files "
    };

    let items: Vec<ListItem> = completer
        .entries
        .iter()
        .take(max_rows)
        .enumerate()
        .map(|(i, name)| {
            let path = completer.dir.join(name);
            let is_dir = path.is_dir();
            let icon = if is_dir { "▸ " } else { "  " };
            let style = if i == selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_dir {
                Style::default().fg(Color::Cyan)
            } else if completer.pick_directory {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(format!("{icon}{name}"), style)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![
                    Span::styled(title, Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!(" {dir_display} "),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
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
