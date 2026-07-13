//! Markdown table grid rendering.
//!
//! Extracted from the main markdown renderer: collecting table cells
//! (via pulldown-cmark's `Table*` events) and rendering the finished
//! grid as plain-text `| … | … |` lines with per-column width capping
//! and optional search highlighting. Everything here is private to
//! the `rendering` module.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::highlight_line_spans;

#[derive(Debug, Default)]
pub(super) struct ListState {
    pub(super) ordered: bool,
    pub(super) number: u64,
}

#[derive(Debug, Default)]
pub(super) struct TableState {
    pub(super) alignments: Vec<pulldown_cmark::Alignment>,
    pub(super) rows: Vec<Vec<String>>,
    pub(super) current_row: Vec<String>,
    pub(super) current_cell: String,
}

impl TableState {
    pub(super) fn start_cell(&mut self) {
        self.current_cell.clear();
    }

    pub(super) fn end_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.push(cell.trim().to_string());
    }

    pub(super) fn end_row(&mut self) {
        if !self.current_row.is_empty() {
            self.rows.push(std::mem::take(&mut self.current_row));
        }
    }
}

/// Render a collected table as plain-text grid lines.
///
/// `width` is the content width in columns; cells are truncated with an
/// ellipsis rather than wrapped, keeping each row one line tall.
pub(super) fn render_table(state: TableState, query: &str, width: usize) -> Vec<Line<'static>> {
    let TableState {
        alignments, rows, ..
    } = state;
    if rows.is_empty() {
        return Vec::new();
    }

    let col_count = alignments
        .len()
        .max(rows.first().map(|r| r.len()).unwrap_or(0));
    let usable_width = width.saturating_sub(col_count.saturating_sub(1) * 3 + 2); // "| " and " |" around cells
    let min_col = 3usize;
    let max_col = if col_count == 0 {
        return Vec::new();
    } else {
        (usable_width / col_count).max(min_col)
    };

    // Compute per-column max width, capped at max_col.
    let mut col_widths = vec![min_col; col_count];
    for row in &rows {
        for (i, cell) in row.iter().enumerate().take(col_count) {
            let visual_len = cell.chars().count();
            col_widths[i] = col_widths[i].max(visual_len).min(max_col);
        }
    }

    let base_style = Style::default().fg(Color::White);
    let mut out: Vec<Line<'static>> = Vec::new();

    // Render separator after header if there is more than one row.
    let has_header = rows.len() > 1;

    for (row_idx, row) in rows.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw("| "));
        for (col, w) in col_widths.iter().enumerate().take(col_count) {
            let cell = row.get(col).map(|s| s.as_str()).unwrap_or("");
            let padded = format_table_cell(cell, *w, alignments.get(col));
            let cell_spans = highlight_line_spans(
                &padded,
                query,
                if row_idx == 0 && has_header {
                    base_style.add_modifier(Modifier::BOLD)
                } else {
                    base_style
                },
            );
            spans.extend(cell_spans);
            spans.push(Span::raw(" | "));
        }
        // Remove the trailing " | " after the last cell and replace with " |".
        if spans.len() >= 2 {
            spans.truncate(spans.len().saturating_sub(1));
            spans.push(Span::raw(" |"));
        }
        out.push(Line::from(spans));

        if row_idx == 0 && has_header {
            let mut sep = String::from("|");
            for w in &col_widths {
                sep.push_str(&"-".repeat(*w));
                sep.push('|');
            }
            out.push(Line::from(Span::styled(
                sep,
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    out
}

fn format_table_cell(
    text: &str,
    width: usize,
    align: Option<&pulldown_cmark::Alignment>,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let visual_len = chars.len();
    if visual_len > width {
        let keep = width.saturating_sub(1);
        let mut s: String = chars.into_iter().take(keep).collect();
        s.push('…');
        return s;
    }
    let pad = width.saturating_sub(visual_len);
    match align {
        Some(pulldown_cmark::Alignment::Center) => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
        Some(pulldown_cmark::Alignment::Right) => {
            format!("{}{}", " ".repeat(pad), text)
        }
        _ => {
            format!("{}{}", text, " ".repeat(pad))
        }
    }
}
