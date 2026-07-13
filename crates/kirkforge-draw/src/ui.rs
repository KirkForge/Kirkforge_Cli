//! UI render function.
//!
//! Builds a scene from `App.state` and paints it into the body pane.
//! The scene is sized to enclose the document bounds with a 1-cell
//! margin so a box drawn at (0,0) doesn't sit flush against the top
//! edge of the pane.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use kirkforge_draw_core::{compose_scene, create_scene, object::get_object_bounds, Rect};

use crate::app::App;
use crate::event::HELP_LINES;
use crate::scene_render::{
    render_resize_handles, render_scene_into, render_selection_marquee, render_text_cursor,
};

/// Build the header-bar text. The leading `*` is the dirty marker the
/// editor toggles every time the document changes; it's the same byte
/// everywhere we surface "modified" (UI, status, future quick-tab title)
/// so read-line tooling can grep for it. Extracted as a pure helper so
/// the four cases (clean/dirty × path/no-path) can be pinned with tests
/// — formatting drift is easy to introduce and invisible in the rendered
/// frame until someone greps the title for `*`.
fn title_text(is_dirty: bool, source_path: Option<&str>) -> String {
    let dirty_marker = if is_dirty { "* " } else { "" };
    match source_path {
        Some(p) => format!(" kfd — {dirty_marker}{p} "),
        None => format!(" kfd — {dirty_marker}"),
    }
}

pub fn draw(app: &mut App, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Header — formatting pinned in `title_text` below.
    let title = title_text(app.state.is_dirty(), app.source_path.as_deref());
    let header = Paragraph::new(Line::from(""))
        .block(Block::default().borders(Borders::BOTTOM).title(title))
        .style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(header, chunks[0]);

    // Body: compose the scene and paint it into the buffer.
    // When a side panel is open, split the main chunk
    // horizontally: body on the left, layers / inspector
    // panels on the right. The body shrinks so mouse hit-tests
    // don't pick up clicks on the panels. The two panels are
    // independent — both can be open at once (body | layers |
    // inspector), each takes a fixed 22-cell column.
    let (body_area, layers_area, inspector_area) = if app.show_layers && app.show_inspector {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(22),
                Constraint::Length(22),
            ])
            .split(chunks[1]);
        (main_chunks[0], Some(main_chunks[1]), Some(main_chunks[2]))
    } else if app.show_layers {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(22)])
            .split(chunks[1]);
        (main_chunks[0], Some(main_chunks[1]), None)
    } else if app.show_inspector {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(22)])
            .split(chunks[1]);
        (main_chunks[0], None, Some(main_chunks[1]))
    } else {
        (chunks[1], None, None)
    };
    app.body_area = body_area;
    app.layers_area = layers_area;
    app.inspector_area = inspector_area;
    let objects = app.state.all_objects();
    let scene = compose_scene_for(&objects);
    app.scene_origin = scene.as_ref().map(|s| s.origin);
    if let Some(scene) = scene {
        render_scene_into(
            &scene,
            frame.buffer_mut(),
            body_area,
            app.scroll_x,
            app.scroll_y,
        );
        if let (Some(origin), Some(bounds)) = (app.scene_origin, app.state.selection_bounds()) {
            render_selection_marquee(
                frame.buffer_mut(),
                body_area,
                app.scroll_x,
                app.scroll_y,
                origin,
                bounds,
            );
            // Handle markers only on a single selected box — lines,
            // text, and multi-select don't have grab handles.
            let sel = app.state.selected();
            if sel.len() == 1 && matches!(sel[0], kirkforge_draw_core::DrawObject::Box(_)) {
                render_resize_handles(
                    frame.buffer_mut(),
                    body_area,
                    app.scroll_x,
                    app.scroll_y,
                    origin,
                    bounds,
                );
            }
        }
        // Live marquee overlay: while the user is mid-drag in Select
        // tool, render the rect's dotted perimeter each frame so
        // they can see what they're about to select. Anchor +
        // current are normalized into a Rect (the user can drag in
        // any direction). Rendered after the static selection
        // marquee so a marquee over an existing selection still
        // shows the live drag rect.
        if let (Some(origin), Some(m)) = (app.scene_origin, app.marquee.as_ref()) {
            let r = kirkforge_draw_core::Rect {
                left: m.anchor.x.min(m.current.x),
                top: m.anchor.y.min(m.current.y),
                right: m.anchor.x.max(m.current.x),
                bottom: m.anchor.y.max(m.current.y),
            };
            render_selection_marquee(
                frame.buffer_mut(),
                body_area,
                app.scroll_x,
                app.scroll_y,
                origin,
                r,
            );
        }
        // F2 text-edit cursor: paint an inverted block at the
        // buffer-end cell so the user can see where the next
        // keystroke will land. Rendered after the selection
        // marquee + live drag rect so the cursor is always on top
        // of selection feedback (the F2 session is single-object
        // so the selected Text is the target).
        if let (Some(origin), Some(edit)) = (app.scene_origin, app.text_edit.as_ref()) {
            if let Some(target) = app.state.text_object(&edit.target_id) {
                let cursor = kirkforge_draw_core::text_edit_cursor_position(
                    target,
                    &edit.buffer,
                    edit.cursor_offset,
                );
                render_text_cursor(
                    frame.buffer_mut(),
                    body_area,
                    app.scroll_x,
                    app.scroll_y,
                    origin,
                    cursor,
                );
            }
        }
    } else {
        let body = Paragraph::new(Line::from("  (empty document — draw something)"))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(body, body_area);
    }

    // Layers panel (right sidebar). Pure data comes from
    // `layer_list(state)` — the renderer just formats rows.
    // The panel is hidden when `show_layers` is false (the
    // body then reclaims the full width).
    if let Some(panel_area) = layers_area {
        render_layers_panel(frame, panel_area, &app.state, app.layer_focus);
    }

    // Inspector panel (right sidebar). Pure data comes from
    // `selection_summary(state)` + `format_summary` — the
    // renderer just formats the strings. The panel is hidden
    // when `show_inspector` is false. Multi-selection shows
    // a placeholder; an empty selection shows a different
    // placeholder. The single-selection summary mirrors
    // the inline status-bar echo so the user sees the same
    // payload whether they have the panel open or not.
    if let Some(panel_area) = inspector_area {
        render_inspector_panel(frame, panel_area, &app.state);
    }

    // Status bar.
    let status_text = if app.find_active() {
        // Find-mode prompt: `find: <query>█ (N matches)`. Takes
        // the whole bar so the user can see what they've typed
        // and how many hits the document has — same pattern as
        // the palette prompt above (the alternative is a
        // floating overlay, which adds a panel worth of
        // complexity for a feature that already has a status
        // line).
        let n = app.find_match_count();
        format!(
            " find: {}\u{2588} ({}{})",
            app.find_query(),
            n,
            if n == 1 { " match" } else { " matches" }
        )
    } else if app.palette_active() {
        // Palette prompt takes the whole bar so the user can see what
        // they're typing. Match list is intentionally inline (not a
        // popup) — six entries fit in a terminal width without
        // wrapping.
        let prompt_char = match app.palette.as_ref().map(|p| p.trigger) {
            Some(crate::app::PaletteTrigger::Colon) => ":",
            Some(crate::app::PaletteTrigger::Slash) => "/",
            None => ":",
        };
        let matches = kirkforge_draw_core::filter_palette(app.palette_buffer());
        let names: Vec<&str> = matches.iter().map(|(n, _)| *n).collect();
        format!(
            " {}{} {} | matches: {}",
            prompt_char,
            app.palette_buffer(),
            app.status,
            if names.is_empty() {
                "(no matches)".to_string()
            } else {
                names.join(", ")
            }
        )
    } else if let Some(summary) = kirkforge_draw_core::selection_summary(&app.state) {
        // Single selection: surface the properties inspector
        // inline in the status bar so the user can see what
        // they've selected without opening a panel. Empty /
        // multi selection falls through to the default status
        // text below.
        format!(
            " {} | tool={:?} | scroll=({}, {}) | objects={} | {}",
            app.status,
            app.state.tool,
            app.scroll_x,
            app.scroll_y,
            app.state.document.objects.len(),
            kirkforge_draw_core::format_summary(&summary),
        )
    } else {
        format!(
            " {} | tool={:?} | scroll=({}, {}) | objects={}",
            app.status,
            app.state.tool,
            app.scroll_x,
            app.scroll_y,
            app.state.document.objects.len(),
        )
    };
    let status = Paragraph::new(Line::from(status_text))
        .style(Style::default().bg(Color::Indexed(236)).fg(Color::White));
    frame.render_widget(status, chunks[2]);

    // Help overlay: last so it paints on top of body + status. Toggled
    // by `?` from the event loop; HELP_LINES is the single source of
    // truth for both the key-map doc comment in event.rs and the
    // rendered text.
    if app.show_help {
        render_help_overlay(frame, frame.area());
    }
}

/// Render a centered help rect. Width and height are derived from the
/// longest line and the number of lines; the rect is clamped to the
/// terminal area so it stays visible on small panes.
fn render_help_overlay(frame: &mut Frame, area: ratatui::layout::Rect) {
    // Width = longest help line + 2-cell border on each side.
    // The clamp is on the TOTAL (text + borders) so the rect
    // stays inside `area` on narrow terminals; clamping the
    // text-only width first and adding the borders after lets
    // the rect overflow by up to 4 cells when the terminal is
    // narrower than the longest help line.
    let width = (HELP_LINES
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(20)
        + 4)
    .min(area.width as usize) as u16;
    let height = (HELP_LINES.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = ratatui::layout::Rect::new(x, y, width, height);

    // Clear the cells underneath so the body / status glyphs don't
    // bleed through the overlay.
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" help — press ? or Esc to close ")
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let lines: Vec<Line> = HELP_LINES.iter().map(|l| Line::from(*l)).collect();
    let para = Paragraph::new(lines)
        .block(block)
        .style(Style::default().fg(Color::White));
    frame.render_widget(para, rect);
}

/// Render the layers panel on the right sidebar. Each row is
/// `kind_label id` with a leading `▶` for selected objects so
/// the user can see at a glance which row maps to the active
/// selection.
///
/// ponytail: side panel — width is fixed by the caller (22
/// cells). The renderer trims the id column to fit. Keyboard
/// navigation (`Up` / `Down` to focus a row, `Enter` to select,
/// `Esc` to clear focus) is wired in `event.rs`'s
/// `cycle_layer_focus` / `commit_layer_focus` / `clear_layer_focus`
/// helpers — the renderer just paints `app.layer_focus` if set.
/// Mouse row-clicking through `handle_layer_click` handles
/// click-to-select on the same rows. When the panel is shorter
/// than the document, scrolling is still a future tick (mouse
/// wheel / PgUp-PgDn on the panel) — today the user sees the
/// topmost N rows.
fn render_layers_panel(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &kirkforge_draw_core::DrawState,
    focus: Option<usize>,
) {
    let layers = kirkforge_draw_core::layer_list(state);
    // ID column width: total width minus the kind label + the
    // selection marker + spacing. The kind label is the widest
    // of "Box", "Line", "Elbow", "Paint", "Text" (5 chars).
    let id_width = (area.width as usize).saturating_sub(8).max(4);
    let mut lines: Vec<Line> = Vec::with_capacity(layers.len() + 1);
    // Header row so the panel reads as a list, not just a stack
    // of lines.
    lines.push(
        Line::from(" layers ").style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Yellow),
        ),
    );
    for (idx, layer) in layers.iter().enumerate() {
        let marker = if layer.selected { "▶ " } else { "  " };
        let kind = kirkforge_draw_core::kind_label(layer.kind);
        // Truncate the id from the right so the row stays in
        // its column. Future tick can show a hover tooltip for
        // the full id; today the status-bar inspector carries
        // it.
        let id_trimmed: String = if layer.id.chars().count() > id_width {
            // Take the first id_width chars; this loses the
            // tail of long ids, but ids are short in practice
            // (8-char hex generated by new_object_id).
            layer.id.chars().take(id_width).collect()
        } else {
            layer.id.clone()
        };
        // Focus + selection compose: focus wins visually (the
        // user navigated here), and the selection marker in
        // the text already shows whether this id is selected.
        // ponytail: focus uses REVERSED to match ratatui's
        // "this is the cursor" idiom. The cyan selection
        // color stays so a focused-but-not-selected row is
        // distinguishable from a focused-and-selected row.
        let style = if focus == Some(idx) {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if layer.selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(format!("{marker}{kind} {id_trimmed}")).style(style));
    }
    if layers.is_empty() {
        lines.push(Line::from(" (empty) ").style(Style::default().fg(Color::DarkGray)));
    }
    let block = Block::default()
        .borders(Borders::LEFT)
        .style(Style::default().bg(Color::Indexed(236)).fg(Color::White));
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}

/// Render the properties inspector on the right sidebar. The
/// data path is: `selection_summary(state) -> Option<
/// SelectionSummary>` (core, untested here — covered by the
/// core-side suite) then `format_summary(&summary) -> String`
/// (core, ditto). The bin-side job is to lay those strings
/// out as panel rows and to substitute the right placeholder
/// when the summary is absent (empty or multi selection).
///
/// ponytail: side panel — width is fixed by the caller (22
/// cells). The renderer wraps the formatted summary at the
/// inner content width (minus a 2-cell gutter for the block
/// border). Word-wrap on the long summary string keeps every
/// row on the panel — `Paragraph` does the wrap for us.
fn render_inspector_panel(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &kirkforge_draw_core::DrawState,
) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(
        Line::from(" inspector ").style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Yellow),
        ),
    );
    let count = state.selected_count();
    if count == 0 {
        lines.push(Line::from(" (no selection) ").style(Style::default().fg(Color::DarkGray)));
    } else if count > 1 {
        // Multi-selection: the core helper deliberately
        // returns None so the status bar falls through to the
        // default text. The panel is more useful with a
        // real placeholder than a blank, so we surface a
        // count + "many selected" message.
        lines.push(
            Line::from(format!(" ({count} selected) ")).style(Style::default().fg(Color::DarkGray)),
        );
    } else if let Some(summary) = kirkforge_draw_core::selection_summary(state) {
        // `format_summary_rows` returns one `String` per field
        // (id / kind / z / color / bounds / kind-specific /
        // parent). Pushing each as a separate `Line` keeps the
        // field labels aligned on the left and avoids the
        // mid-token wraps the old `format_summary` Paragraph
        // produced on a 22-cell panel. A row that overflows the
        // panel's inner width still wraps, but as one coherent
        // block rather than as a random mid-field break.
        for row in kirkforge_draw_core::format_summary_rows(&summary) {
            lines.push(Line::from(format!(" {row} ")).style(Style::default().fg(Color::White)));
        }
    }
    let block = Block::default()
        .borders(Borders::LEFT)
        .style(Style::default().bg(Color::Indexed(236)).fg(Color::White));
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Build a scene sized to enclose every object's bounds with a 1-cell
/// margin. Returns `None` when the document is empty — the caller
/// falls back to a placeholder line.
fn compose_scene_for(
    objects: &[kirkforge_draw_core::DrawObject],
) -> Option<kirkforge_draw_core::Scene> {
    let bounds: Option<Rect> = objects
        .iter()
        .filter_map(get_object_bounds)
        .fold(None, |acc, r| {
            Some(match acc {
                None => r,
                Some(prev) => Rect {
                    left: prev.left.min(r.left),
                    top: prev.top.min(r.top),
                    right: prev.right.max(r.right),
                    bottom: prev.bottom.max(r.bottom),
                },
            })
        });
    let r = bounds?;
    let width = r.right - r.left + 1 + 2;
    let height = r.bottom - r.top + 1 + 2;
    let origin = kirkforge_draw_core::Point {
        x: r.left - 1,
        y: r.top - 1,
    };
    let mut scene = create_scene(width, height, origin);
    compose_scene(&mut scene, objects);
    Some(scene)
}

#[cfg(test)]
mod tests {
    use super::title_text;

    // Pins the four-case title format. The dirty marker is the same
    // byte everywhere we surface "modified" so external tooling
    // (status-line, future tab title) can grep for it; drifting the
    // separator from `— ` to `- ` or moving the `*` would silently
    // break that grep.

    #[test]
    fn title_clean_with_path_has_trailing_space() {
        assert_eq!(
            title_text(false, Some("plan.td.json")),
            " kfd — plan.td.json ",
        );
    }

    #[test]
    fn title_clean_without_path_keeps_one_trailing_space() {
        // The trailing space comes from the literal in the format
        // string (`"— {dirty_marker}"`), not from the marker itself,
        // so all four cases end with exactly one space — consistent.
        assert_eq!(title_text(false, None), " kfd — ");
    }

    #[test]
    fn title_dirty_with_path_inserts_marker_before_path() {
        assert_eq!(
            title_text(true, Some("plan.td.json")),
            " kfd — * plan.td.json ",
        );
    }

    #[test]
    fn title_dirty_without_path_ends_with_marker_and_trailing_space() {
        // Trailing space comes from the marker (`"* "`), matching
        // the convention the with-path variants use.
        assert_eq!(title_text(true, None), " kfd — * ");
    }
}
