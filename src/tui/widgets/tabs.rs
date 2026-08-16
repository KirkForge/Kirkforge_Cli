//! Overlay panel renderers for the former F1–F6 views + the top header.
//!
//! WO 34.1 killed the persistent tab bar. The top of the screen is now
//! a one-line header (`render_header`): app name + current model + a
//! ready/busy indicator. The former tab content renderers below are
//! unchanged — they render as overlays on top of the chat surface when
//! `ActiveTab != None`, summoned via the command palette (Ctrl+K) or
//! direct Ctrl-shortcuts.

use crate::tui::app::{AppState, ConnectionState};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

/// Render the top header — a one-line strip: app name + current model +
/// ready/busy indicator. Replaces the former F1–F6 tab bar (WO 34.1).
pub fn render_header(f: &mut Frame, area: Rect, state: &AppState) {
    let mut spans: Vec<Span> = Vec::new();
    // App name
    spans.push(Span::styled(
        " kf-code",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" │ "));
    // Model + connection state
    let model_span = match &state.provider.connection {
        ConnectionState::Connected { model, .. } => {
            Span::styled(format!("◆ {model}"), Style::default().fg(Color::Green))
        }
        ConnectionState::Disconnected | ConnectionState::Connecting => {
            Span::styled("⚡ Disconnected", Style::default().fg(Color::Red))
        }
        ConnectionState::Error(e) => {
            Span::styled(format!("✗ {e}"), Style::default().fg(Color::Red))
        }
    };
    spans.push(model_span);
    spans.push(Span::raw(" │ "));
    // Ready / busy indicator
    let busy = state.generation.is_generating
        || state.generation.persona_in_progress.is_some()
        || state.generation.workflow_in_progress.is_some();
    if busy {
        spans.push(Span::styled(
            format!("⟳ busy {}", state.spinner_char()),
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled("● ready", Style::default().fg(Color::Green)));
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

/// Render the Jobs tab (F4) as a structured job monitor.
///
/// Parses `cached_jobs_output` into structured rows with status icons
/// (● running, ✓ done, ✗ failed, ⊘ cancelled). Interactive: ↑↓
/// selects, Enter shows details (`/jobs <id>`), C cancels, L shows
/// logs. When `cached_jobs_output` is `None`, a placeholder prompts the
/// user to load.
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
        let parsed = parse_job_rows(cached);
        if parsed.bg_rows.is_empty() && parsed.sched_rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No jobs running.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // ── Background jobs ──────────────────────────────────
            if !parsed.bg_rows.is_empty() {
                lines.push(Line::from(Span::styled(
                    " Background",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                for row in &parsed.bg_rows {
                    lines.push(Line::from(vec![
                        row.icon_span(),
                        Span::raw(format!(" #{}", row.id)),
                        Span::styled(
                            format!("  {}", row.status_text()),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(format!("  {}", truncate(&row.command, 40))),
                    ]));
                }
                lines.push(Line::from(""));
            }

            // ── Scheduled jobs ──────────────────────────────────
            if !parsed.sched_rows.is_empty() {
                lines.push(Line::from(Span::styled(
                    " Scheduled",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                for row in &parsed.sched_rows {
                    let icon = if row.enabled {
                        Span::styled(" ●", Style::default().fg(Color::Green))
                    } else {
                        Span::styled(" ⊘", Style::default().fg(Color::DarkGray))
                    };
                    lines.push(Line::from(vec![
                        icon,
                        Span::raw(format!(" {}", row.id)),
                        Span::styled(
                            format!("  {} | next: {}", row.schedule, row.next),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  Press Enter or switch to this tab to load job status",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Enter: details  |  C: cancel  |  L: logs  |  /jobs clean",
        Style::default().fg(Color::DarkGray),
    )));

    render_interactive(f, area, lines, state);
}

// ── Jobs: structured-row parser (WO 34.6) ──────────────────────────────
//
// Parses `cached_jobs_output` (the text from `refresh_jobs_output`) into
// structured rows. The parser is deliberately conservative: lines that
// don't match the expected prefixes are skipped, so a format change in
// `format_job_status` / `handle_scheduled_list` doesn't blank the tab.
// ponytail: the parser is coupled to the output format of
// `format_job_status` (jobs.rs) and `handle_scheduled_list` (jobs.rs).
// If either changes its line format, this parser needs updating. The
// text fallback (lines that don't match are dropped, not shown) is the
// safety net — a format drift shows fewer rows, not a broken tab.
// Upgrade path: expose a structured `Vec<JobRow>` from the jobs module
// directly so the renderer doesn't parse text at all.

/// Parsed jobs output — split into background and scheduled sections.
struct ParsedJobs {
    bg_rows: Vec<BgJobRow>,
    sched_rows: Vec<SchedJobRow>,
}

/// A parsed background-job row.
struct BgJobRow {
    id: String,
    status: JobStatusIcon,
    command: String,
}

/// A parsed scheduled-job row.
struct SchedJobRow {
    id: String,
    enabled: bool,
    schedule: String,
    next: String,
}

/// Status icon for a background job. Mirrors the emoji in
/// `format_job_status` but uses ASCII-ish symbols the WO spec names.
#[derive(Clone, Copy)]
enum JobStatusIcon {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatusIcon {
    fn as_str(self) -> &'static str {
        match self {
            JobStatusIcon::Running => "●",
            JobStatusIcon::Done => "✓",
            JobStatusIcon::Failed => "✗",
            JobStatusIcon::Cancelled => "⊘",
        }
    }

    fn color(self) -> Color {
        match self {
            JobStatusIcon::Running => Color::Yellow,
            JobStatusIcon::Done => Color::Green,
            JobStatusIcon::Failed => Color::Red,
            JobStatusIcon::Cancelled => Color::DarkGray,
        }
    }

    fn label(self) -> &'static str {
        match self {
            JobStatusIcon::Running => "running",
            JobStatusIcon::Done => "done",
            JobStatusIcon::Failed => "failed",
            JobStatusIcon::Cancelled => "cancelled",
        }
    }
}

impl BgJobRow {
    fn icon_span(&self) -> Span<'static> {
        Span::styled(
            format!(" {} ", self.status.as_str()),
            Style::default().fg(self.status.color()),
        )
    }

    fn status_text(&self) -> String {
        self.status.label().to_string()
    }
}

/// Truncate a command string to `max` chars, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Parse `cached_jobs_output` into structured rows.
fn parse_job_rows(cached: &str) -> ParsedJobs {
    let mut bg_rows = Vec::new();
    let mut sched_rows = Vec::new();
    let mut in_sched = false;

    for line in cached.lines() {
        let trimmed = line.trim_start();
        // Section headers
        if trimmed.starts_with("Background jobs:") {
            in_sched = false;
            continue;
        }
        if trimmed.starts_with("Scheduled jobs:") {
            in_sched = true;
            continue;
        }
        if trimmed.starts_with("No background jobs.")
            || trimmed.starts_with("No scheduled jobs.")
            || trimmed.starts_with("Tip:")
            || trimmed.is_empty()
        {
            continue;
        }

        if in_sched {
            // Scheduled row: "  <id> [enabled] <cron> | <kind> | next: <t> | last: <s>"
            if let Some(row) = parse_sched_row(trimmed) {
                sched_rows.push(row);
            }
        } else {
            // Background row: "  ⏳ running #5 — <cmd>" etc.
            if let Some(row) = parse_bg_row(trimmed) {
                bg_rows.push(row);
            }
        }
    }

    ParsedJobs {
        bg_rows,
        sched_rows,
    }
}

/// Parse a background-job line. The format from `format_job_status` is:
///   `⏳ running #5`, `✅ completed #5 (exit 0)`, `❌ failed #5: <err>`,
///   `🚫 cancelled #5`
/// followed by ` — <command>` (appended in `handle_background_jobs_command`).
fn parse_bg_row(line: &str) -> Option<BgJobRow> {
    // Detect the status emoji/symbol prefix.
    let (status, rest) = if let Some(r) = line.strip_prefix("⏳ running #") {
        (JobStatusIcon::Running, r)
    } else if let Some(r) = line.strip_prefix("✅ completed #") {
        (JobStatusIcon::Done, r)
    } else if let Some(r) = line.strip_prefix("❌ failed #") {
        (JobStatusIcon::Failed, r)
    } else if let Some(r) = line.strip_prefix("🚫 cancelled #") {
        (JobStatusIcon::Cancelled, r)
    } else {
        return None;
    };

    // `rest` is now "5 (exit 0) — cargo test" or "5: <err> — cargo test"
    // or "5 — cargo test". Split on " — " to get the id + status suffix
    // and the command.
    let (id_part, command) = match rest.split_once(" — ") {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };

    // The id is the leading number; the rest is the status suffix
    // ("(exit 0)" / ": <err>" / "").
    let id = id_part
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("")
        .to_string();

    Some(BgJobRow {
        id,
        status,
        command,
    })
}

/// Parse a scheduled-job line. The format from `handle_scheduled_list` is:
///   `<id> [enabled|disabled] <cron> | <kind> | next: <time> | last: <status>`
fn parse_sched_row(line: &str) -> Option<SchedJobRow> {
    // The line starts with the id, then "[enabled]" or "[disabled]".
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    if parts.len() < 2 {
        return None;
    }
    let id = parts[0].to_string();
    let rest = parts[1];

    let enabled = rest.starts_with("[enabled]");
    // Strip the [enabled]/[disabled] tag
    let after_tag = match rest.find(']').map(|i| &rest[i + 1..]) {
        Some(s) => s.trim_start(),
        None => rest,
    };

    // Split on " | " to get schedule, kind, next, last
    let segments: Vec<&str> = after_tag.split(" | ").collect();
    let schedule = segments
        .first()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let next = segments
        .iter()
        .find_map(|s| s.strip_prefix("next: ").map(|t| t.to_string()))
        .unwrap_or_else(|| "—".to_string());

    Some(SchedJobRow {
        id,
        enabled,
        schedule,
        next,
    })
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

#[cfg(test)]
mod jobs_parser_tests {
    use super::*;

    #[test]
    fn parse_empty_output() {
        let parsed = parse_job_rows("No background jobs.\nNo scheduled jobs.\n");
        assert!(parsed.bg_rows.is_empty());
        assert!(parsed.sched_rows.is_empty());
    }

    #[test]
    fn parse_background_running_job() {
        let cached = "Background jobs:\n  ⏳ running #5 — cargo test\n\nTip: ...\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "5");
        assert!(matches!(parsed.bg_rows[0].status, JobStatusIcon::Running));
        assert_eq!(parsed.bg_rows[0].command, "cargo test");
    }

    #[test]
    fn parse_background_completed_job() {
        let cached = "Background jobs:\n  ✅ completed #4 (exit 0) — echo hi\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "4");
        assert!(matches!(parsed.bg_rows[0].status, JobStatusIcon::Done));
        assert_eq!(parsed.bg_rows[0].command, "echo hi");
    }

    #[test]
    fn parse_background_failed_job() {
        let cached = "Background jobs:\n  ❌ failed #3: oops — bad cmd\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "3");
        assert!(matches!(parsed.bg_rows[0].status, JobStatusIcon::Failed));
        assert_eq!(parsed.bg_rows[0].command, "bad cmd");
    }

    #[test]
    fn parse_background_cancelled_job() {
        let cached = "Background jobs:\n  🚫 cancelled #2 — sleep 30\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "2");
        assert!(matches!(parsed.bg_rows[0].status, JobStatusIcon::Cancelled));
    }

    #[test]
    fn parse_scheduled_job() {
        let cached = "Scheduled jobs:\n  abc123 [enabled] @hourly | bash: echo hi | next: 2026-08-16T05:00:00Z | last: ok (done)\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.sched_rows.len(), 1);
        assert_eq!(parsed.sched_rows[0].id, "abc123");
        assert!(parsed.sched_rows[0].enabled);
        assert_eq!(parsed.sched_rows[0].schedule, "@hourly");
        assert_eq!(parsed.sched_rows[0].next, "2026-08-16T05:00:00Z");
    }

    #[test]
    fn parse_scheduled_disabled_job() {
        let cached =
            "Scheduled jobs:\n  xyz [disabled] @daily | bash: echo hi | next: — | last: —\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.sched_rows.len(), 1);
        assert!(!parsed.sched_rows[0].enabled);
    }

    #[test]
    fn parse_mixed_sections() {
        let cached = "\
Background jobs:
  ⏳ running #5 — cargo test
  ✅ completed #4 (exit 0) — echo hi

Scheduled jobs:
  abc [enabled] @hourly | bash: echo | next: 2026-08-16T05:00:00Z | last: ok (done)
";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 2);
        assert_eq!(parsed.sched_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "5");
        assert_eq!(parsed.bg_rows[1].id, "4");
        assert_eq!(parsed.sched_rows[0].id, "abc");
    }

    #[test]
    fn parse_unknown_line_is_skipped() {
        let cached = "Background jobs:\n  some random line\n  ⏳ running #1 — cmd\n";
        let parsed = parse_job_rows(cached);
        assert_eq!(parsed.bg_rows.len(), 1);
        assert_eq!(parsed.bg_rows[0].id, "1");
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let result = truncate("abcdefghijklmnopqrstuvwxyz", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('…'));
    }
}
