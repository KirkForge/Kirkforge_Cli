/// Status bar — model info, token counts, connection state.
use crate::tui::app::{ActiveTab, AppState, ConnectionState};
use crate::tui::rendering::{format_budget_indicator, format_duration, format_token_count};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the status bar at the bottom of the screen.
///
/// When on a non-Chat tab, shows the tab label so the user knows
/// which panel is active. The F1 key always returns to Chat.
pub fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    // ── Tab indicator ────────────────────────────────────────────
    // Show active tab label on the left side of the status bar.
    // Chat is the default (no label shown); other tabs get a tag
    // like " F2:Models │" to orient the user.
    let tab_indicator: Vec<Span> = if state.active_tab == ActiveTab::Chat {
        vec![Span::raw(String::new())]
    } else {
        vec![
            Span::styled(
                format!(" {} ", state.active_tab.label()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
        ]
    };

    let left_info = match &state.connection {
        ConnectionState::Disconnected => {
            Span::styled(" ⚡ Disconnected ", Style::default().fg(Color::Red))
        }
        ConnectionState::Connecting => {
            Span::styled(" ⟳ Connecting... ", Style::default().fg(Color::Yellow))
        }
        ConnectionState::Connected { model, .. } => {
            Span::styled(format!(" ◆ {model} "), Style::default().fg(Color::Green))
        }
        ConnectionState::Error(e) => {
            Span::styled(format!(" ✗ {e} "), Style::default().fg(Color::Red))
        }
    };

    let elapsed = format_duration(state.session_started.elapsed().as_secs_f64());
    let cost_str = if state.cumulative_cost > 0.001 {
        format!(" ${:.4}", state.cumulative_cost)
    } else if state.turn_cost > 0.0 {
        format!(" ${:.4}", state.turn_cost)
    } else {
        String::new()
    };
    let skill_count = state.skill_registry.len();
    let skills_str = if skill_count > 0 {
        format!(" {skill_count}sk")
    } else {
        String::new()
    };

    // ── Plugin trust-tier indicator (Phase 2.3) ────────────────────
    let plugin_str = state.plugin_status.as_deref().unwrap_or("");

    // ── Tool call counter (visible between tool calls when spinner is off) ──
    let tool_calls_span: Span = if state.turn_tool_calls > 0 {
        Span::styled(
            format!("🔧×{} ", state.turn_tool_calls),
            Style::default().fg(Color::Cyan),
        )
    } else {
        Span::raw(String::new())
    };

    // ── Budget indicator (v1.2-p6) ─────────────────────────────────
    // If we have both a connected model and a non-zero per-turn
    // prompt size, show "↑12.4K/128K (10%)" with a color that tells
    // the user when /compact is a good idea. Otherwise fall back to
    // the plain "↑12.4K" cumulative display (pre-first-turn, or no
    // model connected, or no max_context_tokens configured).
    let max_ctx = state
        .model_info
        .as_ref()
        .map(|m| m.max_context_tokens)
        .unwrap_or(0);
    let sent_span: Span = if state.last_turn_prompt_tokens > 0 && max_ctx > 0 {
        let (text, color) = format_budget_indicator(state.last_turn_prompt_tokens, max_ctx);
        Span::styled(format!("↑{text} "), Style::default().fg(color))
    } else {
        Span::styled(
            format!("↑{} ", format_token_count(state.tokens_sent)),
            Style::default().fg(Color::DarkGray),
        )
    };
    let received_span = Span::styled(
        format!("↓{} ", format_token_count(state.tokens_received)),
        Style::default().fg(Color::DarkGray),
    );
    let cost_span = Span::styled(cost_str.clone(), Style::default().fg(Color::DarkGray));
    let elapsed_span = Span::styled(elapsed.clone(), Style::default().fg(Color::DarkGray));
    let skills_span: Span = if skills_str.is_empty() {
        Span::raw(String::new())
    } else {
        Span::styled(
            format!("{skills_str} "),
            Style::default().fg(Color::DarkGray),
        )
    };
    let plugin_span: Span = if plugin_str.is_empty() {
        Span::raw(String::new())
    } else {
        Span::styled(format!("{plugin_str} "), Style::default().fg(Color::Yellow))
    };

    // ── Sandbox indicator (v1.2-p12 follow-up) ─────────────────────
    // Shown in the status bar only when PathGuard is unsandboxed.
    let sandbox_span: Span = if state.unsandboxed {
        Span::styled(
            "⚠️ UNSANDBOXED ".to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(String::new())
    };

    // Compute the spacer width from the actual rendered span widths.
    // `Span::content` is the unstyled text length; we use that for
    // layout math and rebuild with the styled spans for display.
    let collapse_span = Span::styled(
        format!(
            " [Ctrl+T: tool collapse {}] ",
            if state.tool_collapsed { "ON" } else { "OFF" }
        ),
        Style::default()
            .fg(if state.tool_collapsed {
                Color::Green
            } else {
                Color::DarkGray
            })
            .bg(Color::Black),
    );
    let separator = Span::styled(" │ ", Style::default().fg(Color::DarkGray));

    // Display-cell width of a span. `chars().count()` undercounts wide
    // emoji (e.g. `⚠️` is 2 cells but 1 char + variation selector), so
    // add 1 for the known sandbox-emoji span when it's the UNSANDBOXED
    // warning. `unicode-width` is not a direct dep here (only transitive
    // via ratatui), so the manual correction stays cheaper than a new
    // dependency.
    let span_width = |span: &Span| {
        let n = span.content.chars().count();
        if span.content.contains('⚠') {
            n + 1
        } else {
            n
        }
    };

    let tab_indicator_width: usize = tab_indicator
        .iter()
        .map(|s| s.content.chars().count())
        .sum();
    let left_len = tab_indicator_width + span_width(&left_info);

    // Right-side spans in render order. The drop loop below mutates a
    // visibility mask over these. Never-drop spans are excluded from
    // the drop candidates list.
    let mut right: Vec<Span> = vec![
        collapse_span,
        sandbox_span,
        tool_calls_span,
        skills_span,
        plugin_span,
        sent_span,
        received_span,
        cost_span,
        separator,
        elapsed_span,
    ];

    // Drop order: drop first → last. Index into `right`.
    // 4=plugin_span, 3=skills_span, 2=tool_calls_span, 0=collapse_span.
    // Never-drop: 1=sandbox, 5=sent, 6=received, 7=cost, 8=separator, 9=elapsed.
    let drop_order: [usize; 4] = [4, 3, 2, 0];

    let right_width = |right: &[Span]| right.iter().map(|s| span_width(s)).sum::<usize>();
    let fits = |right: &[Span]| area.width as usize >= left_len + right_width(right) + 2;

    // Drop low-priority spans until it fits or only never-drop spans
    // remain. Replace each dropped span with an empty span so indices
    // stay stable.
    if !fits(&right) {
        for &idx in &drop_order {
            if fits(&right) {
                break;
            }
            right[idx] = Span::raw(String::new());
        }
    }

    // Spacer between left and right. If the floor still doesn't fit,
    // collapse to a single space so the overlap is the minimum, not
    // every span piled up.
    let floor = left_len + right_width(&right) + 2;
    let space = if area.width as usize > floor {
        area.width as usize - floor
    } else if area.width as usize > left_len + 1 {
        1
    } else {
        0
    };

    let spacing = " ".repeat(space);

    let mut line_spans = tab_indicator;
    line_spans.push(left_info);
    line_spans.push(Span::styled(spacing, Style::default()));
    line_spans.extend(right);

    let line = Line::from(line_spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::{Duration, Instant};

    fn make_state() -> AppState {
        use std::sync::Arc;
        let config = Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let mut state = AppState::new(config);
        state.connection = ConnectionState::Connected {
            model: "test".into(),
            since: Instant::now(),
        };
        state.session_started = Instant::now() - Duration::from_secs(1);
        state.cumulative_cost = 0.01;
        state
    }

    fn status_row(state: &mut AppState, width: u16) -> String {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_status(f, f.area(), state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut s = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, 0)) {
                s.push_str(cell.symbol());
            }
        }
        s
    }

    /// Regression: the right-side spacer used to omit the Ctrl+T span,
    /// pushing cost/elapsed off-screen on an 80-column terminal.
    #[test]
    fn status_bar_includes_cost_and_elapsed_on_80_cols() {
        let mut state = make_state();
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("1.0s"),
            "elapsed time should be visible on 80-col status bar, got: {row:?}"
        );
        assert!(
            row.contains("$0.0100"),
            "cost should be visible on 80-col status bar, got: {row:?}"
        );
    }

    /// WO 14.4: at narrow widths the plugin span (lowest priority)
    /// drops before cost/elapsed, which are never-drop.
    #[test]
    fn status_bar_drops_plugin_count_below_70_cols() {
        let mut state = make_state();
        state.plugin_status = Some("🔒1".into());
        let wide = status_row(&mut state, 80);
        let narrow = status_row(&mut state, 60);
        assert!(
            wide.contains("🔒"),
            "plugin span should be visible at 80 cols, got: {wide:?}"
        );
        assert!(
            !narrow.contains("🔒"),
            "plugin span should be dropped at 60 cols, got: {narrow:?}"
        );
        assert!(
            narrow.contains("1.0s"),
            "elapsed should survive the drop at 60 cols, got: {narrow:?}"
        );
        assert!(
            narrow.contains("$0.0100"),
            "cost should survive the drop at 60 cols, got: {narrow:?}"
        );
    }

    /// WO 14.4: the UNSANDBOXED warning is never-drop — it stays even
    /// at 40 cols when the session is unsandboxed.
    #[test]
    fn status_bar_keeps_unsandboxed_warning_at_40_cols() {
        let mut state = make_state();
        state.unsandboxed = true;
        let row = status_row(&mut state, 40);
        assert!(
            row.contains("UNSANDBOXED"),
            "UNSANDBOXED warning must stay at 40 cols, got: {row:?}"
        );
    }

    /// WO 14.4: at 60 cols cost appears before elapsed (the right-side
    /// spans didn't clip into each other). The current code only
    /// checked `contains`, not rendering order.
    #[test]
    fn status_bar_no_overlap_at_60_cols() {
        let mut state = make_state();
        let row = status_row(&mut state, 60);
        let cost = row.find("$0.0100");
        let elapsed = row.find("1.0s");
        assert!(
            cost.is_some() && elapsed.is_some(),
            "both cost and elapsed should be present at 60 cols, got: {row:?}"
        );
        assert!(
            cost.unwrap() < elapsed.unwrap(),
            "cost should render before elapsed at 60 cols (no overlap), got: {row:?}"
        );
    }
}
