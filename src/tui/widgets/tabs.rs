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

/// Render the Models tab (F2) as a chooser list + details section.
///
/// The chooser is the primary view: a radio list of available models
/// with ●/○ indicators. The current model is marked with ● and a
/// "Current" label. Provider + context window are shown per model.
/// Below the chooser, a details section surfaces adapter routing,
/// cache, token usage, and cost for the currently-selected model.
///
/// Available models come from what's already in memory: the connected
/// model (from `ConnectionState`) and the configured default model.
/// Runtime discovery of the full Ollama tag list is async and deferred
/// (see the `ponytail:` comment on `model_chooser_rows`) — the chooser
/// covers the common "am I on the right model?" question with the two
/// models the user can actually act on.
///
/// Interactive: ↑/↓ navigates, Enter switches model (runs
/// `/model <name>` via the existing `handle_tab_enter` Models branch),
/// Esc returns to Chat.
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

    // Connection banner (kept — it's the at-a-glance status)
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

    // ── Chooser ────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        " Choose model  (↑↓ navigate, Enter switches)",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    let config = crate::shared::read_shared_config(&state.services.config);
    let rows = model_chooser_rows(state, &config);
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No models available. Set default_model in config or connect to Ollama.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for row in &rows {
            let marker = if row.is_current {
                Span::styled(" ●", Style::default().fg(Color::Green))
            } else {
                Span::styled(" ○", Style::default().fg(Color::DarkGray))
            };
            let label = if row.is_current {
                Span::styled(" Current", Style::default().fg(Color::Green))
            } else {
                Span::raw("")
            };
            let name = Span::raw(format!(" {:<28}", row.name));
            let meta = Span::styled(
                format!("  {} · {}", row.provider, row.context),
                Style::default().fg(Color::DarkGray),
            );
            lines.push(Line::from(vec![marker, name, meta, label]));
        }
    }
    lines.push(Line::from(""));

    // ── Details (for the selected row, or the current model) ──────
    lines.push(Line::from(Span::styled(
        " Details",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    // The selected row index from tab_list_state; fall back to the
    // current model row so details always show something useful.
    let sel_idx = state.ui.tab_list_state.unwrap_or(0);
    // The chooser header + blank + connection banner eat the first few
    // rows; clamp the selection to the chooser rows so we don't index
    // past them.
    let detail_row = rows
        .get(sel_idx.saturating_sub(CHOOSER_HEADER_ROWS))
        .or_else(|| rows.iter().find(|r| r.is_current))
        .or_else(|| rows.first());

    if let Some(row) = detail_row {
        lines.push(Line::from(format!("   Model:    {}", row.name)));
        lines.push(Line::from(format!("   Provider: {}", row.provider)));
        lines.push(Line::from(format!("   Context:  {}", row.context)));
    } else if let Some(info) = &state.provider.model_info {
        lines.push(Line::from(format!("   Model:    {}", info.name)));
        lines.push(Line::from(format!(
            "   Context:  {} tokens",
            crate::tui::rendering::format_token_count(info.max_context_tokens)
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "   No model connected.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Adapter routing
    let routing = &config.model.adapter_routing;
    if routing.is_empty() {
        lines.push(Line::from(Span::styled(
            "   Routing:  (none — using defaults)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from("   Routing:"));
        for (prefix, kind) in routing {
            lines.push(Line::from(format!("     {prefix} → {kind}")));
        }
    }

    // Cache + tokens + cost
    lines.push(Line::from(format!(
        "   Cache:   {} ({} cached tokens, {:.0}% hit)",
        if config.model.cache_enabled {
            "On"
        } else {
            "Off"
        },
        crate::tui::rendering::format_token_count(state.budget.cached_tokens),
        state.budget.cache_hit_ratio * 100.0
    )));
    lines.push(Line::from(format!(
        "   Tokens:  ↑{} ↓{}",
        crate::tui::rendering::format_token_count(state.budget.tokens_sent),
        crate::tui::rendering::format_token_count(state.budget.tokens_received)
    )));
    if state.budget.cumulative_cost > 0.001 {
        lines.push(Line::from(format!(
            "   Cost:    ${:.4}",
            state.budget.cumulative_cost
        )));
    }

    render_interactive(f, area, lines, state);
}

/// Number of rendered lines before the chooser rows in `render_models`.
/// Used to map `tab_list_state` to a chooser row index. If
/// `render_models` changes its header layout, update this constant.
const CHOOSER_HEADER_ROWS: usize = 7;

/// A single row in the model chooser list.
struct ModelChoiceRow {
    name: String,
    provider: String,
    context: String,
    is_current: bool,
}

/// Build the chooser rows from in-memory state. The connected model and
/// the configured default model are the two models the user can act on.
// ponytail: only lists connected + default model. The full Ollama tag
// list is available via `fetch_model_list` (async, lives in
// commands/model.rs) but is not cached on AppState. Adding a cached
// tag-list field + an async refresh on tab-switch is the upgrade path
// when the user needs to pick from more than two models. Ceiling: the
// chooser shows at most 2 rows today; the Enter handler's
// `handle_tab_enter` Models branch already runs `/model <name>` so a
// larger list just needs the rows — no new key handling.
fn model_chooser_rows(state: &AppState, config: &crate::shared::Config) -> Vec<ModelChoiceRow> {
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Connected model first (it's the current one)
    if let crate::tui::app::ConnectionState::Connected { model, .. } = &state.provider.connection {
        if seen.insert(model.clone()) {
            let provider = crate::tui::commands::adapter_kind_for_model(model).to_string();
            let context = state
                .provider
                .model_info
                .as_ref()
                .map(|m| crate::tui::rendering::format_token_count(m.max_context_tokens))
                .unwrap_or_else(|| "—".to_string());
            rows.push(ModelChoiceRow {
                name: model.clone(),
                provider,
                context,
                is_current: true,
            });
        }
    }

    // Configured default model (if different from connected)
    let default = &config.model.default_model;
    if !default.is_empty() && seen.insert(default.clone()) {
        let provider = crate::tui::commands::adapter_kind_for_model(default).to_string();
        rows.push(ModelChoiceRow {
            name: default.clone(),
            provider,
            context: "—".to_string(),
            is_current: false,
        });
    }

    rows
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

// ── Settings: semantic-label helpers (WO 34.4) ─────────────────────────
//
// Pure functions mapping config-struct values to human-readable labels.
// Kept module-private: only `render_settings` and the Enter-handler's
// `settings_keys_and_values` (keys/mod.rs) consume them. The two call
// sites must agree on the row order, so the helpers are the single
// source of truth for the wording.

/// Human label for the command-approval posture. `auto_approve` is the
/// primary gate; `bang_requires_approval` narrows it for `!` commands.
fn approval_label(auto_approve: bool, bang_requires_approval: bool) -> &'static str {
    match (auto_approve, bang_requires_approval) {
        (true, false) => "Auto-approve safe commands",
        (true, true) => "Auto-approve (bang still asks)",
        (false, _) => "Always ask",
    }
}

/// Human label for the sandbox posture. `Some(path)` → "Project root";
/// `None` → "None".
fn sandbox_label(sandbox_dir: Option<&str>) -> &'static str {
    if sandbox_dir.is_some() {
        "Project root"
    } else {
        "None"
    }
}

/// Human label for hidden-file access. `block_dotfiles` is the config
/// field; `true` means dotfiles are blocked.
fn dotfiles_label(block_dotfiles: bool) -> &'static str {
    if block_dotfiles {
        "Blocked"
    } else {
        "Allowed"
    }
}

/// Human label for a boolean tool flag. Reused for dry_run and
/// follow_symlinks so the wording stays consistent.
fn bool_label(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

/// Render the Settings tab (F5).
///
/// Groups settings semantically (MODEL / SAFETY / TOOLS) with
/// human-readable values, then a collapsed "Raw config" section at the
/// bottom for developers. Display only — no edit capability (WO 34.4).
/// Interactive: ↑/↓ selects rows, Enter reports the selected value.
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

    // ── MODEL ──────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        " MODEL",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   Default model:     {}",
        config.model.default_model
    )));
    lines.push(Line::from(format!(
        "   Provider:          {}",
        config.model.anthropic_provider
    )));
    // Context window comes from the connected model_info, not the config
    // (the config has no per-model context field). Fall back to "—".
    let context = state
        .provider
        .model_info
        .as_ref()
        .map(|m| crate::tui::rendering::format_token_count(m.max_context_tokens))
        .unwrap_or_else(|| "—".to_string());
    lines.push(Line::from(format!("   Context window:    {context}")));
    lines.push(Line::from(""));

    // ── SAFETY ─────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        " SAFETY",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   Command approval:  {}",
        approval_label(
            config.security.auto_approve,
            config.security.bang_requires_approval,
        )
    )));
    lines.push(Line::from(format!(
        "   Sandbox:           {}",
        sandbox_label(config.security.sandbox_dir.as_deref())
    )));
    lines.push(Line::from(format!(
        "   Hidden files:      {}",
        dotfiles_label(config.security.block_dotfiles)
    )));
    lines.push(Line::from(""));

    // ── TOOLS ──────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        " TOOLS",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "   Dry run:           {}",
        bool_label(config.tools.dry_run)
    )));
    lines.push(Line::from(format!(
        "   Follow symlinks:   {}",
        bool_label(config.tools.follow_symlinks)
    )));
    lines.push(Line::from(""));

    // ── Raw config (collapsed, for developers) ─────────────────────
    // The original field-name: value pairs, dimmed. Kept in the same
    // order as the old dump so anyone grepping a render dump still finds
    // the field they expect.
    lines.push(Line::from(Span::styled(
        " Raw config",
        Style::default().fg(Color::DarkGray),
    )));
    let raw = raw_config_lines(&config);
    for l in &raw {
        lines.push(Line::from(Span::styled(
            format!("   {l}"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " /reload to apply config changes",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}

/// Build the raw `field: value` lines for the collapsed Settings section
/// and for the Enter-handler's `settings_keys_and_values` lookup. The
/// order MUST match `render_settings`'s interactive rows so Enter maps
/// the selected visual row to the right underlying value.
fn raw_config_lines(config: &crate::shared::Config) -> Vec<String> {
    let mut lines = Vec::new();
    // MODEL group (3 semantic rows above)
    lines.push(format!("default_model: {}", config.model.default_model));
    lines.push(format!(
        "anthropic_provider: {}",
        config.model.anthropic_provider
    ));
    lines.push(format!("cache_enabled: {}", config.model.cache_enabled));
    // SAFETY group (3 semantic rows above)
    lines.push(format!(
        "auto_approve: {} (bang: {})",
        config.security.auto_approve, config.security.bang_requires_approval
    ));
    lines.push(format!(
        "sandbox_dir: {}",
        config.security.sandbox_dir.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!(
        "block_dotfiles: {}",
        config.security.block_dotfiles
    ));
    // TOOLS group (2 semantic rows above)
    lines.push(format!("dry_run: {}", config.tools.dry_run));
    lines.push(format!("follow_symlinks: {}", config.tools.follow_symlinks));
    lines
}

/// Render the Threads tab (F6).
///
/// Shows active forks/sessions with status columns. Fed by the
/// daemon's `ThreadsChanged` push events (WO 17.2/17.9).
pub fn render_threads(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        " Threads",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Session list from the session picker data
    if let Some(ref picker) = state.session.session_picker {
        let count = picker.len();
        if count == 0 {
            lines.push(Line::from(Span::styled(
                " No active sessions",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(Span::raw(format!(" {count} session(s)"))));
            lines.push(Line::from(""));
            // Show up to 20 sessions with status indicators
            for entry in picker.entries().iter().take(20) {
                let status = if entry.path.exists() {
                    Span::styled("●", Style::default().fg(Color::Green))
                } else {
                    Span::styled("○", Style::default().fg(Color::DarkGray))
                };
                lines.push(Line::from(vec![
                    status,
                    Span::raw(format!(" {}", entry.id)),
                ]));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            " No session data loaded",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " /resume <id> to switch to a session",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}
