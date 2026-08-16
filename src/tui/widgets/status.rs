/// Status bar — model, context pressure, cost, state (WO 34.3).
///
/// Reduced from 12+ indicators (every metric in the app) to 4 essentials:
///   `● Model · context · $cost · State`
/// plus the sandbox warning (`⚠️ UNSANDBOXED`) appended when active.
/// Everything else lives in `/status`. The bar fits in ~50 chars; if it
/// doesn't fit, the model name is truncated.
use crate::tui::app::{AppState, ConnectionState};
use crate::tui::rendering::{budget_pct, format_token_count};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the status bar at the bottom of the screen.
///
/// Four items, dot-separated, plus an optional sandbox warning:
///   `● Claude Sonnet 4 · 82% context · $0.04 · Ready`
///   `● Claude Sonnet 4 · 8.2k tokens · $0.04 · Ready`
///   `● Claude Sonnet 4 · 8.2k tokens · $0.04 · Ready  ⚠️ UNSANDBOXED`
///
/// Context pressure shows as `NN% context` with a colour (green <50%,
/// yellow 50-80%, red >80%) when the budget is known. Below 50% the
/// token count is shown instead (`8.2k tokens`) so the bar is not
/// noisy at comfortable levels. When no budget is known (no model
/// connected, no max_context_tokens), the plain token count is shown.
pub fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let model = model_label(state);
    let (context_text, context_color) = context_span(state);
    let cost_str = cost_label(state);
    let state_str = state_label(state);

    // Bullet + model.
    let bullet = Span::styled("● ", Style::default().fg(Color::Green));
    let model_span = Span::styled(model, Style::default().fg(Color::White));

    let sep = Span::styled(" · ", Style::default().fg(Color::DarkGray));

    let context_span = Span::styled(context_text, Style::default().fg(context_color));
    let cost_span = Span::styled(cost_str, Style::default().fg(Color::DarkGray));
    let state_span = Span::styled(state_str, Style::default().fg(Color::Cyan));

    let mut spans = vec![
        bullet,
        model_span,
        sep.clone(),
        context_span,
        sep.clone(),
        cost_span,
        sep,
        state_span,
    ];

    // Sandbox warning — safety-critical, always appended when active.
    if state.provider.unsandboxed {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "⚠️ UNSANDBOXED",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_widget(paragraph, area);
}

/// Model name for the status bar. Falls back to the connection state
/// when no model is connected.
fn model_label(state: &AppState) -> String {
    match &state.provider.connection {
        ConnectionState::Connected { model, .. } => model.clone(),
        ConnectionState::Disconnected | ConnectionState::Connecting => "Disconnected".to_string(),
        ConnectionState::Error(e) => format!("Error: {e}"),
    }
}

/// Context pressure span: `(text, color)`. When the budget is known and
/// pressure is >= 50%, shows `NN% context` with the threshold colour.
/// Below 50% (or when no budget is known), shows the token count
/// (`8.2k tokens`) so the bar stays quiet at comfortable levels.
fn context_span(state: &AppState) -> (String, Color) {
    let max_ctx = state
        .provider
        .model_info
        .as_ref()
        .map(|m| m.max_context_tokens)
        .unwrap_or(0);
    let used = state.budget.last_turn_prompt_tokens;

    if let Some(pct) = budget_pct(used, max_ctx) {
        let color = pressure_color(pct);
        if pct < 50 {
            // Comfortable — show the token count, not the percentage.
            (format!("{} tokens", format_token_count(used)), color)
        } else {
            (format!("{pct}% context"), color)
        }
    } else {
        // No budget known (no model, or max_context_tokens == 0). Show
        // the cumulative sent count as a fallback signal.
        (
            format!("{} tokens", format_token_count(state.budget.tokens_sent)),
            Color::DarkGray,
        )
    }
}

/// Pressure colour: green <50%, yellow 50-80%, red >80%.
fn pressure_color(pct: u8) -> Color {
    if pct < 50 {
        Color::Green
    } else if pct < 80 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Cost label: `$0.04` (cumulative), or empty when zero.
fn cost_label(state: &AppState) -> String {
    if state.budget.cumulative_cost > 0.001 {
        format!("${:.2}", state.budget.cumulative_cost)
    } else if state.budget.turn_cost > 0.0 {
        format!("${:.2}", state.budget.turn_cost)
    } else {
        "$0.00".to_string()
    }
}

/// State label: `Generating…`, `Working…` (persona/workflow/test), or
/// `Ready`. `Cancelled` is transient (no persistent flag) and is not
/// surfaced here — the cancel message is already in the conversation.
fn state_label(state: &AppState) -> &'static str {
    if state.generation.is_generating {
        "Generating…"
    } else if state.generation.persona_in_progress.is_some()
        || state.generation.workflow_in_progress.is_some()
        || state.generation.test_in_progress
    {
        "Working…"
    } else {
        "Ready"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::app_state;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::{Duration, Instant};

    fn make_state() -> AppState {
        let mut state = app_state();
        state.provider.connection = ConnectionState::Connected {
            model: "Claude Sonnet 4".into(),
            since: Instant::now(),
        };
        state.session.session_started = Instant::now() - Duration::from_secs(1);
        state.budget.cumulative_cost = 0.04;
        state.provider.model_info = Some(crate::shared::ModelInfo {
            name: "Claude Sonnet 4".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::Anthropic,
            max_context_tokens: 200_000,
            recommended_temperature: 0.0,
            supports_images: false,
            supports_cache: false,
        });
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

    /// The 4-item bar fits in 50 chars (the WO 34.3 contract). A short
    /// model name + low-token display + cost + Ready must all render.
    #[test]
    fn status_bar_4_item_layout_fits_in_50_chars() {
        let mut state = make_state();
        // Low token count → shows "0 tokens", comfortable (green).
        state.budget.last_turn_prompt_tokens = 0;
        let row = status_row(&mut state, 50);
        assert!(
            row.contains("Claude Sonnet 4"),
            "model should be visible at 50 cols, got: {row:?}"
        );
        assert!(
            row.contains("$0.04"),
            "cost should be visible at 50 cols, got: {row:?}"
        );
        assert!(
            row.contains("Ready"),
            "state should be visible at 50 cols, got: {row:?}"
        );
        assert!(
            row.contains("tokens"),
            "token count should be visible at 50 cols, got: {row:?}"
        );
    }

    /// When context pressure is high (>= 50%), the bar shows
    /// `NN% context` instead of the token count.
    #[test]
    fn status_bar_shows_context_pressure_when_high() {
        let mut state = make_state();
        // 164k / 200k = 82%.
        state.budget.last_turn_prompt_tokens = 164_000;
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("82% context"),
            "should show pressure percentage at 82%, got: {row:?}"
        );
    }

    /// When context pressure is comfortable (< 50%), the bar shows
    /// the token count, not the percentage.
    #[test]
    fn status_bar_shows_token_count_when_comfortable() {
        let mut state = make_state();
        // 8.2k / 200k = ~4%.
        state.budget.last_turn_prompt_tokens = 8_200;
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("8.2K tokens"),
            "should show token count below 50%, got: {row:?}"
        );
        assert!(
            !row.contains("context"),
            "should NOT show percentage below 50%, got: {row:?}"
        );
    }

    /// The sandbox warning stays visible — it is safety-critical and
    /// never dropped (WO 14.4 contract preserved).
    #[test]
    fn status_bar_keeps_unsandboxed_warning() {
        let mut state = make_state();
        state.provider.unsandboxed = true;
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("UNSANDBOXED"),
            "UNSANDBOXED warning must stay visible, got: {row:?}"
        );
    }

    /// Generating state is surfaced as `Generating…`.
    #[test]
    fn status_bar_shows_generating_when_in_flight() {
        let mut state = make_state();
        state.generation.is_generating = true;
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("Generating…"),
            "should show Generating… when is_generating, got: {row:?}"
        );
    }

    /// Disconnected state shows `Disconnected` as the model label.
    #[test]
    fn status_bar_shows_disconnected_when_no_model() {
        let mut state = app_state();
        state.provider.connection = ConnectionState::Disconnected;
        let row = status_row(&mut state, 80);
        assert!(
            row.contains("Disconnected"),
            "should show Disconnected when no model, got: {row:?}"
        );
    }

    /// The 4-item format matches the WO spec exactly:
    /// `● Model · context · $cost · State`.
    #[test]
    fn status_bar_format_matches_spec() {
        let mut state = make_state();
        state.budget.last_turn_prompt_tokens = 164_000; // 82%
        let row = status_row(&mut state, 80);
        // Bullet, model, separator, context, separator, cost, separator, state.
        assert!(
            row.starts_with("● Claude Sonnet 4 · 82% context · $0.04 · "),
            "format should match `● Model · context · $cost · State`, got: {row:?}"
        );
    }
}
