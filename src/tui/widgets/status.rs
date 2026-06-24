/// Status bar — model info, token counts, connection state.
use crate::tui::app::{AppState, ConnectionState};
use crate::tui::rendering::{format_budget_indicator, format_duration, format_token_count};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the status bar at the bottom of the screen.
pub fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let left_info = match &state.connection {
        ConnectionState::Disconnected => {
            Span::styled(" ⚡ Disconnected ", Style::default().fg(Color::Red))
        }
        ConnectionState::Connecting => {
            Span::styled(" ⟳ Connecting... ", Style::default().fg(Color::Yellow))
        }
        ConnectionState::Connected { model, .. } => {
            Span::styled(format!(" ◆ {} ", model), Style::default().fg(Color::Green))
        }
        ConnectionState::Error(e) => {
            Span::styled(format!(" ✗ {} ", e), Style::default().fg(Color::Red))
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
        format!(" {}sk", skill_count)
    } else {
        String::new()
    };

    // ── Plugin trust-tier indicator (Phase 2.3) ────────────────────
    let plugin_str = state.plugin_status.as_deref().unwrap_or("");

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
        Span::styled(format!("↑{} ", text), Style::default().fg(color))
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
            format!("{} ", skills_str),
            Style::default().fg(Color::DarkGray),
        )
    };
    let plugin_span: Span = if plugin_str.is_empty() {
        Span::raw(String::new())
    } else {
        Span::styled(
            format!("{} ", plugin_str),
            Style::default().fg(Color::Yellow),
        )
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
    let right_visible_len: usize = [
        sandbox_span.content.chars().count(),
        skills_span.content.chars().count(),
        plugin_span.content.chars().count(),
        sent_span.content.chars().count(),
        received_span.content.chars().count(),
        cost_span.content.chars().count(),
        elapsed_span.content.chars().count(),
        // " │ " separator between cost/elapsed
        3,
    ]
    .iter()
    .sum();
    let left_len = left_info.content.chars().count();
    let space = if area.width as usize > left_len + right_visible_len + 2 {
        area.width as usize - left_len - right_visible_len
    } else {
        1
    };

    let spacing = " ".repeat(space);

    let line = Line::from(vec![
        left_info,
        Span::styled(
            " [Ctrl+T: tool collapse ".to_string()
                + if state.tool_collapsed { "ON" } else { "OFF" }
                + "] ",
            Style::default()
                .fg(if state.tool_collapsed {
                    Color::Green
                } else {
                    Color::DarkGray
                })
                .bg(Color::Black),
        ),
        Span::styled(spacing, Style::default()),
        sandbox_span,
        skills_span,
        plugin_span,
        sent_span,
        received_span,
        cost_span,
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        elapsed_span,
    ]);

    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_widget(paragraph, area);
}
