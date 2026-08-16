//! Welcome screen — centered banner shown on a fresh session.
//!
//! Rendered when `messages.is_empty() && input.is_empty()`. Any keystroke
//! into the input dismisses it (the welcome is purely a render gate,
//! not a mode). Ctrl+O opens a directory picker overlay (via
//! FileCompleter in pick_directory mode).

use crate::tui::app::AppState;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the welcome screen in the main content area.
///
/// Layout (top to bottom):
///   - banner `k i r k f o r g e`
///   - subtitle `AI coding assistant for your repository`
///   - CWD
///   - recent sessions (3-5, from `session_picker` if present)
///   - quick actions: `/`, `@`, `Ctrl+K`, `Ctrl+S`
///   - status: `● Ready · <model>`
///
/// Any keystroke into the input dismisses the welcome (the render gate
/// in `render_app` checks `messages.is_empty() && input.is_empty()`).
pub fn render_welcome(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    // Vertical padding to center the block.
    let center_pad = area.height.saturating_sub(14) / 2;
    for _ in 0..center_pad {
        lines.push(Line::from(""));
    }

    // Banner
    lines.push(Line::from(Span::styled(
        "  k i r k f o r g e",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    // Subtitle
    lines.push(Line::from(Span::styled(
        "  AI coding assistant for your repository",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    // CWD
    let cwd = state.ui.cwd.display().to_string();
    lines.push(Line::from(vec![
        Span::styled("  cwd: ", Style::default().fg(Color::DarkGray)),
        Span::styled(cwd, Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(""));

    // Recent sessions (3-5, only if the session picker has entries).
    // The picker is populated by the daemon on startup or by `/resume`;
    // if it's absent or empty we skip the section entirely (no empty
    // header — per WO 34.8 done condition).
    if let Some(ref picker) = state.session.session_picker {
        let entries = picker.entries();
        if !entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Recent sessions",
                Style::default().fg(Color::DarkGray),
            )));
            for entry in entries.iter().take(5) {
                let dot = if entry.path.exists() {
                    Span::styled("●", Style::default().fg(Color::Green))
                } else {
                    Span::styled("○", Style::default().fg(Color::DarkGray))
                };
                lines.push(Line::from(vec![
                    dot,
                    Span::raw(format!(" {}", entry.id)),
                    Span::styled(
                        format!("  · {} msgs", entry.message_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    // Quick actions
    lines.push(Line::from(Span::styled(
        "  Quick actions",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(vec![
        Span::styled("    /  ", Style::default().fg(Color::Yellow)),
        Span::raw("Commands"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    @  ", Style::default().fg(Color::Yellow)),
        Span::raw("Add a file"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    Ctrl+K  ", Style::default().fg(Color::Yellow)),
        Span::raw("Command palette"),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    Ctrl+S  ", Style::default().fg(Color::Yellow)),
        Span::raw("Sessions"),
    ]));
    lines.push(Line::from(""));

    // Status: ● Ready · <model>
    let model_name = state
        .provider
        .model_info
        .as_ref()
        .map(|m| m.name.clone())
        .or_else(|| match &state.provider.connection {
            crate::tui::app::ConnectionState::Connected { model, .. } => Some(model.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "—".to_string());
    lines.push(Line::from(vec![
        Span::styled("  ● Ready", Style::default().fg(Color::Green)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(model_name, Style::default().fg(Color::White)),
    ]));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_to_string(state: &AppState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_welcome(f, f.area(), state))
            .unwrap();
        let mut buf = String::new();
        for y in 0..terminal.size().unwrap().height {
            for x in 0..terminal.size().unwrap().width {
                let cell = terminal.backend().buffer().cell((x, y)).unwrap();
                buf.push_str(cell.symbol());
            }
            buf.push('\n');
        }
        buf
    }

    fn bare_state() -> AppState {
        AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )))
    }

    /// Banner + subtitle are always present.
    #[test]
    fn welcome_shows_banner_and_subtitle() {
        let s = bare_state();
        let rendered = render_to_string(&s, 80, 24);
        assert!(rendered.contains("k i r k f o r g e"), "banner missing");
        assert!(
            rendered.contains("AI coding assistant for your repository"),
            "subtitle missing"
        );
    }

    /// Quick actions section is always present.
    #[test]
    fn welcome_shows_quick_actions() {
        let s = bare_state();
        let rendered = render_to_string(&s, 80, 24);
        assert!(rendered.contains("Commands"), "Commands action missing");
        assert!(rendered.contains("Add a file"), "Add a file missing");
        assert!(
            rendered.contains("Command palette"),
            "Command palette missing"
        );
        assert!(rendered.contains("Sessions"), "Sessions action missing");
    }

    /// Status line shows Ready + model name (or em-dash when no model).
    #[test]
    fn welcome_shows_ready_status() {
        let s = bare_state();
        let rendered = render_to_string(&s, 80, 24);
        assert!(rendered.contains("Ready"), "Ready status missing");
        // No model connected on a bare state → em-dash fallback.
        assert!(rendered.contains("—"), "model fallback missing");
    }

    /// Status line shows the model name when connected.
    #[test]
    fn welcome_shows_model_name_when_connected() {
        let mut s = bare_state();
        s.provider.connection = crate::tui::app::ConnectionState::Connected {
            model: "qwen2.5:0.5b".into(),
            since: std::time::Instant::now(),
        };
        let rendered = render_to_string(&s, 80, 24);
        assert!(
            rendered.contains("qwen2.5:0.5b"),
            "connected model name missing"
        );
    }

    /// Recent sessions section is SKIPPED when the picker is absent
    /// (no empty header — per WO 34.8 done condition).
    #[test]
    fn welcome_omits_recent_sessions_when_no_picker() {
        let s = bare_state();
        assert!(s.session.session_picker.is_none());
        let rendered = render_to_string(&s, 80, 24);
        assert!(
            !rendered.contains("Recent sessions"),
            "Recent sessions header should be absent when no picker"
        );
    }

    /// Recent sessions section appears when the picker has entries.
    #[test]
    fn welcome_shows_recent_sessions_when_picker_has_entries() {
        use crate::session::session_index::SessionEntry;
        use crate::tui::components::session_picker::SessionPicker;
        use std::path::PathBuf;

        let mut s = bare_state();
        let entries = vec![
            SessionEntry {
                id: "2026-08-16-session-01".into(),
                path: PathBuf::from("/tmp/does-not-exist"),
                started_at: "2026-08-16T10:00:00Z".into(),
                message_count: 42,
                size_bytes: 1024,
            },
            SessionEntry {
                id: "2026-08-15-session-03".into(),
                path: PathBuf::from("/tmp/does-not-exist-2"),
                started_at: "2026-08-15T18:00:00Z".into(),
                message_count: 7,
                size_bytes: 256,
            },
        ];
        s.session.session_picker = Some(SessionPicker::new(entries));
        let rendered = render_to_string(&s, 80, 24);
        assert!(
            rendered.contains("Recent sessions"),
            "Recent sessions header missing"
        );
        assert!(
            rendered.contains("2026-08-16-session-01"),
            "first session id missing"
        );
        assert!(rendered.contains("42 msgs"), "message count missing");
    }

    /// CWD line is always present.
    #[test]
    fn welcome_shows_cwd() {
        let s = bare_state();
        let rendered = render_to_string(&s, 80, 24);
        assert!(rendered.contains("cwd:"), "cwd label missing");
    }
}
