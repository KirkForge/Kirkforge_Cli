//! Tab panel renderers for F1–F5 views.
//!
//! Each tab renders its content into the main content area (the top
//! chunk of the vertical layout). The status bar at the bottom shows
//! which tab is active.

use crate::tui::app::{ActiveTab, AppState};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

/// Render the tab bar into the status line, showing F1–F5 labels
/// with the active tab highlighted.
fn tab_bar_spans(active: ActiveTab) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, tab) in ActiveTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ", Style::default()));
        }
        let label = tab.label();
        let style = if *tab == active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
    }
    spans
}

/// Return the tab bar as a `Line` for embedding in the status bar.
pub fn tab_bar_line(active: ActiveTab) -> Line<'static> {
    Line::from(tab_bar_spans(active))
}

/// Render the Models tab (F2).
///
/// Shows the current model name, provider, context window, and
/// adapter routing configuration.
pub fn render_models(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    // Header
    lines.push(Line::from(Span::styled(
        " Models",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Connection info
    match &state.connection {
        crate::tui::app::ConnectionState::Connected { model, since } => {
            lines.push(Line::from(Span::styled(
                format!(" ● Connected: {model}"),
                Style::default().fg(Color::Green),
            )));
            let elapsed = state.session_started.elapsed().as_secs_f64();
            let duration = crate::tui::rendering::format_duration(elapsed);
            lines.push(Line::from(Span::raw(format!("   Uptime: {duration}"))));
            let _ = since; // suppress unused warning
        }
        crate::tui::app::ConnectionState::Disconnected => {
            lines.push(Line::from(Span::styled(
                " ⚡ Disconnected",
                Style::default().fg(Color::Red),
            )));
        }
        crate::tui::app::ConnectionState::Connecting => {
            lines.push(Line::from(Span::styled(
                " ⟳ Connecting...",
                Style::default().fg(Color::Yellow),
            )));
        }
        crate::tui::app::ConnectionState::Error(e) => {
            lines.push(Line::from(Span::styled(
                format!(" ✗ {e}"),
                Style::default().fg(Color::Red),
            )));
        }
    }
    lines.push(Line::from(""));

    // Model info
    if let Some(ref info) = state.model_info {
        lines.push(Line::from(Span::styled(
            " Model Info",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!("   Name:            {}", info.name)));
        lines.push(Line::from(format!(
            "   Context window:  {} tokens",
            crate::tui::rendering::format_token_count(info.max_context_tokens)
        )));
        lines.push(Line::from(""));
    }

    // Adapter routing
    let config = crate::shared::read_shared_config(&state.config);
    let routing = &config.model.adapter_routing;
    if routing.is_empty() {
        lines.push(Line::from(Span::styled(
            " Adapter Routing: (none — using defaults)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " Adapter Routing",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (prefix, kind) in routing {
            lines.push(Line::from(format!("   {prefix} → {kind}")));
        }
        lines.push(Line::from(""));
    }

    // Token usage
    lines.push(Line::from(Span::styled(
        " Token Usage",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   Sent:     {}",
        crate::tui::rendering::format_token_count(state.tokens_sent)
    )));
    lines.push(Line::from(format!(
        "   Received: {}",
        crate::tui::rendering::format_token_count(state.tokens_received)
    )));
    if state.cumulative_cost > 0.001 {
        lines.push(Line::from(format!(
            "   Cost:     ${:.4}",
            state.cumulative_cost
        )));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Render the Plugins tab (F3).
///
/// Shows loaded plugins with their trust tier and status.
pub fn render_plugins(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Plugins",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let config = crate::shared::read_shared_config(&state.config);
    let sources = &config.tools.plugin_sources;
    let enabled = &config.tools.enabled_plugins;
    let disabled = &config.tools.disabled_plugins;

    if sources.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No plugins configured",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (name, path) in sources {
            let is_disabled = disabled.contains(name);
            let is_enabled = enabled.contains(name) && !is_disabled;
            let status = if is_disabled {
                Span::styled(" OFF", Style::default().fg(Color::Red))
            } else if is_enabled {
                Span::styled(" ON ", Style::default().fg(Color::Green))
            } else {
                Span::styled(" — ", Style::default().fg(Color::DarkGray))
            };
            let path_str = path.display().to_string();
            lines.push(Line::from(vec![
                status,
                Span::raw(format!(" {name}")),
                Span::styled(
                    format!(" ({path_str})"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Use /plugins toggle <name> to enable/disable at runtime",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        " Use /plugins list for full details",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Render the Jobs tab (F4).
///
/// Shows background and scheduled job status.
pub fn render_jobs(f: &mut Frame, area: Rect, _state: &AppState) {
    let lines = vec![
        Line::from(Span::styled(
            " Jobs",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  No active jobs",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Use /jobs to list scheduled jobs",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            " Use /jobs schedule <cron> <prompt> to create one",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Render the Settings tab (F5).
///
/// Shows key configuration values from the loaded config.
pub fn render_settings(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Settings",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let config = crate::shared::read_shared_config(&state.config);

    // Model settings
    lines.push(Line::from(Span::styled(
        " Model",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   default_model:       {}",
        config.model.default_model
    )));
    lines.push(Line::from(format!(
        "   ollama_host:         {}",
        config.model.ollama_host
    )));
    lines.push(Line::from(format!(
        "   anthropic_provider:  {}",
        config.model.anthropic_provider
    )));
    lines.push(Line::from(format!(
        "   cache_enabled:       {}",
        config.model.cache_enabled
    )));
    lines.push(Line::from(""));

    // Security settings
    lines.push(Line::from(Span::styled(
        " Security",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   auto_approve:         {}",
        config.security.auto_approve
    )));
    lines.push(Line::from(format!(
        "   sandbox_dir:          {}",
        config.security.sandbox_dir.as_deref().unwrap_or("(none)")
    )));
    lines.push(Line::from(format!(
        "   block_dotfiles:       {}",
        config.security.block_dotfiles
    )));
    lines.push(Line::from(format!(
        "   bang_requires_approval: {}",
        config.security.bang_requires_approval
    )));
    lines.push(Line::from(""));

    // Tool settings
    lines.push(Line::from(Span::styled(
        " Tools",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   dry_run:              {}",
        config.tools.dry_run
    )));
    lines.push(Line::from(format!(
        "   follow_symlinks:      {}",
        config.tools.follow_symlinks
    )));
    lines.push(Line::from(format!(
        "   max_tool_calls_per_turn: {}",
        config.tools.max_tool_calls_per_turn
    )));
    lines.push(Line::from(""));

    // Session settings
    lines.push(Line::from(Span::styled(
        " Session",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   carryover_enabled:    {}",
        config.session.carryover_enabled
    )));
    lines.push(Line::from(format!(
        "   worktree_enabled:     {}",
        config.session.worktree_enabled
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Use /reload to apply config changes after editing .kf-code/config.toml",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}
