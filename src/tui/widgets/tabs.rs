//! Tab panel renderers for F1–F6 views and the top tab bar.
//!
//! Each tab renders its content into the main content area (the top
//! chunk of the vertical layout). A persistent tab bar at the very top
//! shows F1–F6 labels with the active one highlighted.

use crate::tui::app::{ActiveTab, AppState};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

/// Render the top tab bar — a one-line strip showing F1–F6 labels.
/// The active tab is highlighted; inactive tabs are dim.
pub fn render_tab_bar(f: &mut Frame, area: Rect, state: &AppState) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, tab) in ActiveTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let label = tab.label();
        let is_active = tab == &state.ui.active_tab;
        let style = if is_active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Black));
    f.render_widget(paragraph, area);
}

/// Render a List widget from lines with optional interactive selection.
/// When `state.ui.tab_list_state` is `Some(idx)`, the list is rendered with
/// a highlight on the selected row; ↑/↓ navigation and Enter/Space
/// invocation are handled by the key handler in `keys/mod.rs`.
fn render_interactive(f: &mut Frame, area: Rect, lines: Vec<Line<'_>>, state: &AppState) {
    let items: Vec<ListItem> = lines.into_iter().map(ListItem::new).collect();
    let count = items.len();
    let list = List::new(items).highlight_style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let mut list_state = ListState::default();
    if let Some(sel) = state.ui.tab_list_state {
        list_state.select(Some(sel.min(count.saturating_sub(1))));
    }
    f.render_stateful_widget(list, area, &mut list_state);
}

/// Render the Models tab (F2).
///
/// Shows the current model name, provider, context window, and
/// adapter routing configuration. Interactive when the tab has focus:
/// ↑/↓ moves selection, Enter/Space on a model row runs `/model <name>`.
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
    match &state.provider.connection {
        crate::tui::app::ConnectionState::Connected { model, since } => {
            lines.push(Line::from(Span::styled(
                format!(" ● Connected: {model}"),
                Style::default().fg(Color::Green),
            )));
            let elapsed = state.session.session_started.elapsed().as_secs_f64();
            let duration = crate::tui::rendering::format_duration(elapsed);
            lines.push(Line::from(Span::raw(format!("   Uptime: {duration}"))));
            let _ = since; // suppress unused warning
        }
        crate::tui::app::ConnectionState::Disconnected
        | crate::tui::app::ConnectionState::Connecting => {
            lines.push(Line::from(Span::styled(
                " ⚡ Disconnected",
                Style::default().fg(Color::Red),
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
    if let Some(ref info) = state.provider.model_info {
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
    let config = crate::shared::read_shared_config(&state.services.config);
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
        crate::tui::rendering::format_token_count(state.budget.tokens_sent)
    )));
    lines.push(Line::from(format!(
        "   Received: {}",
        crate::tui::rendering::format_token_count(state.budget.tokens_received)
    )));
    if state.budget.cumulative_cost > 0.001 {
        lines.push(Line::from(format!(
            "   Cost:     ${:.4}",
            state.budget.cumulative_cost
        )));
    }

    render_interactive(f, area, lines, state);
}

/// Render the Plugins tab (F3).
///
/// Shows loaded plugins with their trust tier and status. Interactive:
/// ↑/↓ selects a plugin row, Enter/Space toggles it via `/plugins toggle`.
pub fn render_plugins(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Plugins",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let config = crate::shared::read_shared_config(&state.services.config);
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

    render_interactive(f, area, lines, state);
}

/// Render the Jobs tab (F4).
///
/// Shows background and scheduled job status. Interactive: ↑/↓ selects
/// rows, Enter/Space on a hint row runs the corresponding `/jobs` command.
/// When `cached_jobs_output` is `Some`, its content is rendered directly;
/// otherwise a placeholder is shown.
pub fn render_jobs(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Jobs",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if let Some(ref cached) = state.session.cached_jobs_output {
        for line in cached.lines() {
            lines.push(Line::from(Span::raw(format!(" {line}"))));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  Press Enter or switch to this tab to load job status",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " /jobs <id> for detail  |  /jobs <id> cancel  |  /jobs clean",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}

/// Render the Settings tab (F5).
///
/// Shows key configuration values from the loaded config. Interactive:
/// ↑/↓ selects rows, Enter/Space on the /reload hint invokes it.
pub fn render_settings(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Settings",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let config = crate::shared::read_shared_config(&state.services.config);

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
        " /reload to apply config changes",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}

/// Render the Sessions tab (F6).
///
/// Shows two subsections:
///   - **RECENT**: recent sessions with timestamp + message count, fed by
///     the daemon's `ThreadsChanged` push events (WO 17.2/17.9) via the
///     session picker.
///   - **FORKS**: forks of the current session (from `ForkManager`).
pub fn render_sessions(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Sessions",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // ── RECENT subsection ──────────────────────────────────────────
    let recent_count = state
        .session
        .session_picker
        .as_ref()
        .map(|p| p.len())
        .unwrap_or(0);
    if recent_count > 0 {
        lines.push(Line::from(Span::styled(
            " RECENT",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(ref picker) = state.session.session_picker {
            for entry in picker.entries().iter().take(20) {
                let status = if entry.path.exists() {
                    Span::styled("●", Style::default().fg(Color::Green))
                } else {
                    Span::styled("○", Style::default().fg(Color::DarkGray))
                };
                lines.push(Line::from(vec![
                    status,
                    Span::raw(format!(" {}", entry.id)),
                    Span::styled(
                        format!("  · {} msgs", entry.message_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(Span::styled(
            " No recent sessions",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
    }

    // ── FORKS subsection ───────────────────────────────────────────
    let fork_count = state
        .session
        .fork_manager
        .as_ref()
        .map(|fm| fm.list().len())
        .unwrap_or(0);
    if fork_count > 0 {
        lines.push(Line::from(Span::styled(
            " FORKS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(ref fm) = state.session.fork_manager {
            for fork in fm.list().iter().take(20) {
                lines.push(Line::from(vec![
                    Span::styled("↳ ", Style::default().fg(Color::Cyan)),
                    Span::raw(fork.id.as_str()),
                    Span::styled(
                        format!("  — {}", fork.label),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        " /resume <id> to switch  ·  /fork to branch  ·  /sessions to manage",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}
