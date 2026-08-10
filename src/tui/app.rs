/// Main application state and event handling.
use crate::session::session_fork::ForkManager;
use crate::session::skills::SkillRegistry;
use crate::shared::{ModelInfo, SharedConfig};
use crossterm::event::KeyCode;
use kf_plugin_host::PluginRegistry;
use ratatui::text::Line;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

pub use crate::tui::commands::{handle_workflow_command, WorkflowHandle};

/// Represents the connection state for the status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    // Reserved for async connection transitions; all rendering paths
    // already handle it, so keep the variant even though it is not
    // currently emitted.
    Connecting,
    Connected { model: String, since: Instant },
    Error(String),
}

/// Cached rendered lines for the chat panel.
///
/// `entries` stores one entry per message in `AppState::messages`. Each entry
/// records the message's render-generation version at the time it was rendered
/// and the resulting `Line`s (header + body, not the trailing blank
/// separator). The cache is invalidated when rendering parameters change:
/// terminal width, search query, or tool-collapse state.
#[derive(Debug, Default)]
pub struct ChatRenderCache {
    pub content_width: usize,
    pub search_query: String,
    pub tool_collapsed: bool,
    pub expanded_tools: HashSet<usize>,
    pub collapsed_messages: HashSet<usize>,
    /// Per-message render cache: for each message, `Some((version, rendered_lines))`
    /// if the entry has been rendered at least once; `None` otherwise. `version` is
    /// the value of [`ConversationEntry::version`] when the entry was rendered.
    pub entries: Vec<Option<(u64, Vec<Line<'static>>)>>,
}

impl ChatRenderCache {
    /// Drop all cached entries but keep the parameter snapshot.
    pub fn clear_entries(&mut self) {
        self.entries.clear();
    }

    /// True if the cached parameter snapshot still matches the current state.
    pub fn params_match(
        &self,
        content_width: usize,
        search_query: &str,
        tool_collapsed: bool,
        expanded_tools: &HashSet<usize>,
        collapsed_messages: &HashSet<usize>,
    ) -> bool {
        self.content_width == content_width
            && self.search_query == search_query
            && self.tool_collapsed == tool_collapsed
            && self.expanded_tools == *expanded_tools
            && self.collapsed_messages == *collapsed_messages
    }

    /// Store the current parameters as the new cache snapshot.
    pub fn snapshot_params(
        &mut self,
        content_width: usize,
        search_query: &str,
        tool_collapsed: bool,
        expanded_tools: &HashSet<usize>,
        collapsed_messages: &HashSet<usize>,
    ) {
        self.content_width = content_width;
        self.search_query = search_query.to_string();
        self.tool_collapsed = tool_collapsed;
        self.expanded_tools = expanded_tools.clone();
        self.collapsed_messages = collapsed_messages.clone();
    }
}

/// Active tab for the TUI panel system.
///
/// F1–F5 switch between panels. F6 opens the Threads view.
/// The Chat tab is the default and reproduces the existing
/// single-panel layout. Other tabs replace the main content area
/// with a dedicated panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveTab {
    /// F1 — Conversation view (default, existing chat panel)
    #[default]
    Chat,
    /// F2 — Model / adapter info and switching
    Models,
    /// F3 — Plugin list, status, toggle
    Plugins,
    /// F4 — Scheduled and background job status
    Jobs,
    /// F5 — Config display and live reload
    Settings,
    /// F6 — Threads overview (forks + sessions)
    Threads,
}

impl ActiveTab {
    /// All tabs in F-key order.
    pub const ALL: [ActiveTab; 6] = [
        ActiveTab::Chat,
        ActiveTab::Models,
        ActiveTab::Plugins,
        ActiveTab::Jobs,
        ActiveTab::Settings,
        ActiveTab::Threads,
    ];

    /// F-key label for the tab bar (e.g. "F1:Chat").
    pub fn label(&self) -> &'static str {
        match self {
            ActiveTab::Chat => "F1:Chat",
            ActiveTab::Models => "F2:Models",
            ActiveTab::Plugins => "F3:Plugins",
            ActiveTab::Jobs => "F4:Jobs",
            ActiveTab::Settings => "F5:Settings",
            ActiveTab::Threads => "F6:Sessions",
        }
    }

    /// Map an F-key code to a tab, or return None for non-F keys.
    pub fn from_key_code(code: KeyCode) -> Option<ActiveTab> {
        match code {
            KeyCode::F(1) => Some(ActiveTab::Chat),
            KeyCode::F(2) => Some(ActiveTab::Models),
            KeyCode::F(3) => Some(ActiveTab::Plugins),
            KeyCode::F(4) => Some(ActiveTab::Jobs),
            KeyCode::F(5) => Some(ActiveTab::Settings),
            KeyCode::F(6) => Some(ActiveTab::Threads),
            _ => None,
        }
    }
}

/// Slash-command popup filter state.
///
/// When the user types `/` and the popup is active, `query` holds the
/// filter text after the `/`. Arrow keys move `selected`; Enter inserts
/// the selected command into the input buffer.
#[derive(Debug, Clone, Default)]
pub struct SlashMenu {
    pub query: String,
    pub selected: usize,
}

/// File-completer popup state for @-mention path browsing.
///
/// `dir` is the currently-browsed directory, `entries` are its filtered
/// children, `selected` is the highlight row, and `query` is the
/// text after `@` up to the cursor.
#[derive(Debug, Clone, Default)]
pub struct FileCompleter {
    pub dir: std::path::PathBuf,
    pub entries: Vec<String>,
    pub selected: usize,
    pub query: String,
    /// When true, Enter on a directory confirms it as the new cwd
    /// (Ctrl+O directory-pick mode) instead of inserting the path
    /// into the input buffer.
    pub pick_directory: bool,
}

/// Search state — extracted from AppState (WO 22.6 R3).
#[derive(Debug, Default)]
pub struct SearchState {
    /// When `true`, the input box is being used as a search bar.
    /// Ctrl+F enters search mode; typing filters the chat
    /// conversation; Enter commits and leaves the matches
    /// highlighted; Esc cancels and clears the matches.
    pub mode: bool,
    /// The current search query (built up while in search mode).
    /// Empty when not searching.
    pub query: String,
    /// All match positions in the conversation, in document order.
    /// Each entry is `(message_index, byte_offset, source)` for the
    /// start of the match in `messages[message_index].content` or
    /// `messages[message_index].tool_output` (see
    /// `crate::tui::search::SearchSource`). Filled in when search is
    /// committed; cleared on cancel or `/clear`.
    pub matches: Vec<crate::tui::search::MatchPos>,
    /// Index into `search.matches` of the currently-highlighted
    /// match. `n` cycles forward, `N` (Shift+N) cycles backward.
    /// When `search.matches.is_empty()`, this is meaningless.
    pub match_idx: usize,
}

/// Application state — single source of truth for the TUI.
///
/// # ponytail: deferred — AppState decomposition (WO 20.6.0 U1)
///
/// This struct has ~66 fields. Splitting into sub-structs (ApprovalState,
/// SearchState, DaemonState, RenderCache, GenerationState) would improve
/// readability and reduce borrow-checker workarounds in the render path.
/// However, the fields are accessed across 15+ files (keys, events,
/// approval_keys, components, commands) and the borrow patterns are tightly
/// coupled (e.g. `state.pending_approval.take()` + `state.approval_scroll`
/// in the same closure). A safe decomposition requires auditing every
/// access pattern — too coupled for a single session. Upgrade path:
/// start with `RenderCache` (already a separate struct) and `SearchState`
/// (self-contained fields, few cross-concern accesses), then progressively
/// extract the others.
pub struct AppState {
    /// Conversation messages
    pub messages: VecDeque<ConversationEntry>,

    /// Current user input buffer
    pub input: String,
    /// Cursor position as a Unicode **char index** (not byte offset).
    /// This is safe across UTF-8 multi-byte characters. Convert to byte
    /// offset via [`cursor_byte()`] before any string slicing.
    pub cursor_position: usize,

    /// Connection
    pub connection: ConnectionState,
    pub model_info: Option<ModelInfo>,

    /// Scroll position for the chat view.
    /// 0 = top of content. Max = bottom (latest messages).
    /// When auto_scroll is true, scroll_offset is reset to max
    /// each render cycle so the user always sees the latest messages.
    pub scroll_offset: usize,

    /// If true, the view automatically follows new content to the bottom.
    /// Set false when the user scrolls up; re-enabled when they scroll
    /// back to the bottom.
    pub auto_scroll: bool,

    /// Maximum valid scroll offset, set each render in widgets/chat.rs.
    /// Used by key handlers (PgUp/PgDn/Up/Down) to clamp scroll_offset
    /// *before* the next render so off-by-N flashes are avoided.
    pub max_scroll: usize,

    /// Thinking panel (collapsible)
    pub thinking_panel_visible: bool,
    pub thinking_buffer: Vec<String>,

    /// Tool call status
    pub pending_approval: Option<PendingApproval>,

    /// Token counters
    pub tokens_sent: usize,
    pub tokens_received: usize,

    /// Cost tracking
    pub turn_cost: f64,
    pub cumulative_cost: f64,

    /// Session start time
    pub session_started: Instant,

    /// Shared config reference. Kept behind an `Arc<RwLock>` so that
    /// SIGHUP/`/reload` can update live behavior without restarting.
    pub config: SharedConfig,

    /// Skill registry for slash commands (loaded from SKILL.md files)
    pub skill_registry: SkillRegistry,

    // ── Session forking (Phase 7) ───────────────────────────
    /// Path to the conversation NDJSON log file.
    pub log_path: Option<PathBuf>,
    /// Session display ID (e.g. "2026-06-03-session-01").
    pub session_id: String,
    /// Fork manager for creating and listing conversation forks.
    pub fork_manager: Option<ForkManager>,

    // ── Session exit (Phase 17) ─────────────────────────────
    /// Set to true to break the event loop and trigger carryover save.
    pub should_exit: bool,

    // ── Generation state ────────────────────────────────────
    /// True while the model is generating a response (between Enter and Done).
    pub is_generating: bool,
    /// Tool calls made so far in the current executor turn. Reset when a
    /// turn completes (CostStats). Shown in the status bar so the user
    /// can see progress even when is_generating is false between tool calls.
    pub turn_tool_calls: usize,

    /// Fork-isolated subagent currently running in the background.
    pub persona_in_progress: Option<crate::tui::commands::PersonaHandle>,
    /// Cancel flag for the running persona, checked between internal turns.
    pub persona_cancel: Option<Arc<AtomicBool>>,

    /// Spinner frame counter — cycles through a spinner animation
    /// to show the model is thinking before the first token arrives.
    pub spinner_tick: u64,

    /// Set of background job IDs that have already been notified as completed.
    /// Used to avoid repeated notifications for the same job.
    pub notified_jobs: std::collections::HashSet<u64>,

    /// Set of scheduled-job run IDs that have already been notified as
    /// completed. Each scheduled job overwrites its `last_run`, so this tracks
    /// run IDs (not job IDs) so every run is announced exactly once.
    pub notified_scheduled_runs: std::collections::HashSet<String>,

    // ── Tool output collapse (v1.1) ───────────────────────────────
    /// When true, long tool entries are collapsed to a one-line summary.
    /// Toggled with Ctrl+T. Default true so the chat view is never flooded
    /// by default — users opt in to the full flood.
    pub tool_collapsed: bool,

    /// Per-index expansion override: even when `tool_collapsed` is true,
    /// an entry whose index is in this set renders in full. Allows users
    /// to expand specific tool results they want to inspect.
    pub expanded_tools: std::collections::HashSet<usize>,

    // ── Chat render geometry cache (Step 6 of TUI chat polish) ───────
    /// Cached `Line`s per message so streaming only recomputes the last
    /// assistant message instead of the whole conversation.
    pub chat_render_cache: ChatRenderCache,

    /// Last content width used by `render_chat`, in columns. Search
    /// navigation needs the same wrap width the renderer used so it
    /// can compute a matching scroll offset after expanding a tool
    /// card or jumping to a match.
    pub last_content_width: usize,

    // ── Per-message collapse (TUI v2) ────────────────────────────────
    /// Indices of conversation entries that the user has collapsed.
    /// Collapsed messages show only the header + an expand hint. Default
    /// is expanded for every message.
    pub collapsed_messages: HashSet<usize>,

    // ── Approval dialog scroll (v1.2-p11) ──────────────────────────────
    /// Vertical scroll offset into the args preview, in lines.
    /// 0 = top of args. Set by the approval-mode key handler
    /// (PageUp/PageDown/Up/Down/Home/End). Reset to 0 in
    /// `drain_approval_requests` whenever a new approval arrives.
    /// Lives on AppState (not PendingApproval) so a deny-then-replace
    /// cycle naturally re-zeroes it via the existing take/replace path.
    pub approval_scroll: usize,

    /// Max valid scroll offset for the current approval's args preview.
    /// Set each render in `render_approval_dialog` from the actual
    /// wrapped-line count minus the visible window. Used by the
    /// key handler to clamp scroll BEFORE the next render (same
    /// off-by-N pattern as `max_scroll` for the chat view).
    pub approval_max_scroll: usize,

    /// Toggle between unified diff and side-by-side diff in the
    /// approval dialog. Side-by-side needs at least 80 columns, so
    /// the renderer falls back to unified when the terminal is too
    /// narrow even if this flag is true.
    pub approval_diff_side_by_side: bool,

    // ── Budget indicator (v1.2-p6) ─────────────────────────────────
    /// The prompt token count of the most recent turn.
    ///
    /// This is the **per-turn** value (NOT a running sum) — the API
    /// reports `prompt_tokens` per response, and the TUI mirrors the
    /// last reported value into this field. The status bar uses it
    /// to compute the budget-pressure percentage:
    ///   `last_turn_prompt_tokens / model_info.max_context_tokens`.
    ///
    /// Why per-turn, not cumulative: the model sees the *whole
    /// conversation* on every turn, so the per-turn prompt size is
    /// the right "current context pressure" metric. A cumulative sum
    /// of all per-turn prompts would be N times too large.
    ///
    /// Initialised to 0 (pre-first-turn). The status bar treats 0 as
    /// "no signal yet" and falls back to the plain `↑N` display.
    pub last_turn_prompt_tokens: usize,

    // ── Prompt-cache indicator (P3-6) ─────────────────────────────
    /// Cumulative cache-read tokens reported by the adapter across the
    /// session. Mirrors the adapter's `cached_tokens` usage field and is
    /// surfaced in the status bar so the operator can verify KV-cache
    /// reuse for the prompt-cache stem.
    pub cached_tokens: usize,
    /// Estimated size of the stable prompt-cache stem (system prompt +
    /// tool definitions) in tokens. Updated each turn by `TurnEvent::CacheStats`.
    pub stem_tokens: usize,
    /// Latest per-turn cache-hit ratio (`cached_tokens / prompt_tokens`).
    /// Surfaced in the status bar as a percentage.
    pub cache_hit_ratio: f64,

    // ── Bang approval gate (review.md arch concern #1) ─────────────
    /// When `Some`, the user has typed `!` with `bang_requires_approval`
    /// enabled, and is being shown the approval dialog for the local
    /// (no-model) bash run. `None` in the common case. Mirrors
    /// `pending_approval` in shape but doesn't go through the executor's
    /// oneshot channel — bang is a pure local feature.
    pub pending_bang: Option<PendingBangCommand>,

    // ── Conversation search (review.md gap #4) ─────────────
    pub search: SearchState,

    // ── Code-block copy cycle (P3) ────────────────────────────────
    /// `Ctrl+Shift+B` cycles through the code blocks of the most
    /// recent assistant message. This counter tracks which block is
    /// copied next; it wraps around when it reaches the number of
    /// blocks in that message.
    pub code_block_copy_index: usize,

    // ── /test command (review.md gap #9) ─────────────────────
    /// True while a `/test` command is running. Used to (1) gate the
    /// input box against stacking tests, (2) drive the spinner in
    /// place of the model-generation spinner.
    pub test_in_progress: bool,

    // ── Recent-session picker (daemon follow-up) ────────────
    /// When set, the TUI is showing the recent-session picker overlay
    /// instead of the normal input box. Triggered at startup (if the
    /// daemon has recent sessions and no explicit resume flag was given)
    /// or by `/resume` with no arguments inside a running session.
    pub session_picker: Option<crate::tui::components::session_picker::SessionPicker>,

    // ── /undo stack (review.md gap #7) ───────────────────────
    /// Shared undo stack. The executor owns the write side (push via
    /// `edit_file` / `write_file`); the TUI uses it read-only for
    /// `/undo list` and `/undo count`. `None` when the stack could
    /// not be created at session start.
    pub undo_stack: Option<crate::tools::UndoStackRef>,

    // ── Plugin trust-tier status (Phase 2.3) ──────────────────────
    /// Compact summary of loaded plugin trust tiers, displayed in the
    /// status bar. Example: "🔒2 ⚡1". `None` when no plugins are loaded.
    pub plugin_status: Option<String>,

    // ── Runtime plugin registry (Phase 11) ────────────────────────────
    /// In-TUI copy of the active plugin registry. Mutated by `/plugins`
    /// commands and forwarded to the executor over the plugin-reload
    /// channel so the toolset/hook/verifier view updates between turns.
    pub plugin_registry: PluginRegistry,

    // ── PathGuard sandbox indicator (v1.2-p12 follow-up) ─────────────
    /// If true, the session is intentionally unsandboxed. The TUI chat
    /// banner and status bar surface this so the operator sees the
    /// posture, not just a tracing log line.
    pub unsandboxed: bool,

    // ── Frame-pacing v2: render-on-state-change ───────────────────
    /// Set to `true` whenever `state` mutates in a way that should
    /// produce a redraw. The event loop checks this flag at the top
    /// of each iteration and skips `terminal.draw` when it's still
    /// `false` (i.e. the previous frame is up-to-date and there's
    /// been no new input).
    ///
    /// The flag is reset to `false` immediately after a successful
    /// render. Every site that mutates `state` in a way visible to
    /// the renderer — stream events, approvals, key handling, the
    /// 4Hz slow-tick that drives the spinner — must call
    /// `mark_dirty()` to schedule the next frame.
    ///
    /// This replaces the earlier "render every iteration, sleep
    /// 16ms" pattern (the 2026-06-11 fix at `tui/mod.rs:412-429`).
    /// The 16ms cap was good enough to bring CPU from 100% to ~5%
    /// per session, but it burned cycles re-rendering identical
    /// frames. Render-on-state-change is a tighter bound: zero
    /// frames when nothing's happening, plus a 4Hz slow-tick when
    /// the spinner is animating.
    pub dirty: bool,

    // ── Ollama pull progress (gap #22) ──────────────────────────
    /// Latest pull-progress event received from `/api/pull`. Used by
    /// the renderer to draw a progress bar in the chat panel. `None`
    /// when no pull is in progress.
    pub pull_progress: Option<PullProgress>,

    // ── Programmable workflows (WO-4) ─────────────────────────────
    /// Handle to the workflow currently running in the background.
    pub workflow_in_progress: Option<crate::tui::commands::WorkflowHandle>,
    /// Cancel flag for the running workflow, checked between steps.
    pub workflow_cancel: Option<Arc<AtomicBool>>,

    // ── Doom loop detection (WO 8.2) ──────────────────────────────
    /// Set when the executor reports a doom loop (same tool failing
    /// the same way N turns in a row). `Some` with `count >= 3 &&
    /// !acknowledged` triggers the warning banner; user action
    /// (break/plan/continue) sets `acknowledged = true` so the
    /// banner hides without clearing the underlying state.
    pub doom_loop: Option<DoomLoopState>,
    /// Banner highlight position. Independent of `DoomLoopState`
    /// so the user can move the highlight before committing an
    /// action. Lives on AppState (not DoomLoopState) because
    /// resetting the underlying state on a successful tool call
    /// shouldn't lose the user's current selection.
    pub doom_loop_selection: crate::tui::widgets::doom_banner::DoomLoopSelection,

    // ── Tab-completion suggestions (WO 14.6) ───────────────────────
    /// One-line completion list shown above/below the input when Tab
    /// produces multiple matches (slash commands or @-mention paths).
    /// Empty when there is nothing to suggest. The key handler fills
    /// it on Tab; any other keypress clears it. Rendered as a dim
    /// hint line in `widgets/input.rs`.
    pub completion_suggestions: Vec<String>,

    // ── Tab panel system ──────────────────────────────────────────
    /// Currently active tab. F1–F6 switch tabs; the Chat tab (F1)
    /// is the default and shows the conversation view.
    pub active_tab: ActiveTab,

    /// Row selection state for the active tab panel (Models, Plugins,
    /// Jobs, Settings, Threads). `None` for Chat (no selectable rows).
    pub tab_list_state: Option<usize>,

    // ── Continuation round indicator (WO 23.9-R3) ──────────────
    /// When `Some((round, max))`, the executor is in a `FinishReason::Length`
    /// continuation loop. The status bar renders "⟳ round/max" in Yellow.
    /// Cleared when the turn completes normally (CostStats).
    pub continuation: Option<(usize, usize)>,

    // ── Daemon push events (WO 17.2) ───────────────────────────────
    /// When true, the daemon pushed `ThreadsChanged` and the TUI should
    /// re-list recent sessions on the next draw tick instead of polling.
    pub sessions_dirty: bool,
    /// When true, the daemon pushed `JobsChanged` and the Jobs tab should
    /// re-read scheduled jobs on the next draw tick.
    pub jobs_dirty: bool,
    /// Cached formatted output for the Jobs tab, refreshed when `jobs_dirty`
    /// is set. Rendered directly by the Jobs tab widget.
    pub cached_jobs_output: Option<String>,
    /// Shared flags set by the daemon event reader task. The TUI event
    /// loop drains these into the local `sessions_dirty` / `jobs_dirty`
    /// fields on each iteration so the render path never blocks.
    #[cfg(unix)]
    pub daemon_flags:
        Option<std::sync::Arc<std::sync::Mutex<crate::tui::daemon_events::DaemonEventFlags>>>,

    // ── Slash-command popup ──────────────────────────────────────────
    /// When `Some`, a filterable popup listing slash commands is shown
    /// above the input bar. Filled when the user types `/`; dismissed
    /// on Esc, Enter, or loss of focus.
    pub slash_menu: Option<SlashMenu>,

    // ── @-mention file completer popup ───────────────────────────────
    /// When `Some`, a directory-browsing popup for @-mentions is shown
    /// above the input bar. Dismissed on Esc or loss of focus.
    pub file_completer: Option<FileCompleter>,

    /// Current working directory. Updated by Ctrl+O directory picker.
    pub cwd: std::path::PathBuf,
}

/// Snapshot of a detected doom loop. Held on `AppState` so the
/// banner widget can render without re-querying the executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomLoopState {
    /// Number of consecutive identical tool errors so far.
    pub count: usize,
    /// Name of the tool that kept failing.
    pub tool: String,
    /// Truncated text of the most recent error.
    pub last_error: String,
    /// Set by the banner's key handler when the user picks one of
    /// the three actions (break / plan / continue). The banner
    /// hides once acknowledged; the count remains visible in
    /// `doom_loop` until a successful tool call resets the
    /// executor-side tracker.
    pub acknowledged: bool,
}

/// Snapshot of an in-progress Ollama model pull.
#[derive(Debug, Clone, PartialEq)]
pub struct PullProgress {
    pub status: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

impl AppState {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            messages: VecDeque::new(),
            input: String::new(),
            cursor_position: 0,
            connection: ConnectionState::Disconnected,
            model_info: None,
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            thinking_panel_visible: false,
            thinking_buffer: Vec::new(),
            pending_approval: None,
            tokens_sent: 0,
            tokens_received: 0,
            turn_cost: 0.0,
            cumulative_cost: 0.0,
            session_started: Instant::now(),
            config,
            skill_registry: SkillRegistry::new(),
            log_path: None,
            session_id: String::new(),
            fork_manager: None,
            should_exit: false,
            is_generating: false,
            turn_tool_calls: 0,
            persona_in_progress: None,
            persona_cancel: None,
            spinner_tick: 0,
            notified_jobs: std::collections::HashSet::new(),
            notified_scheduled_runs: std::collections::HashSet::new(),
            tool_collapsed: true,
            expanded_tools: std::collections::HashSet::new(),
            collapsed_messages: HashSet::new(),
            chat_render_cache: ChatRenderCache::default(),
            last_content_width: 0,
            approval_scroll: 0,
            approval_max_scroll: 0,
            approval_diff_side_by_side: false,
            last_turn_prompt_tokens: 0,
            cached_tokens: 0,
            stem_tokens: 0,
            cache_hit_ratio: 0.0,
            pending_bang: None,
            search: SearchState::default(),
            code_block_copy_index: 0,
            test_in_progress: false,
            undo_stack: None,
            session_picker: None,
            plugin_status: None,
            plugin_registry: PluginRegistry::new(),
            unsandboxed: false,
            // Start dirty so the first frame draws immediately (the
            // connection banner / status bar are non-empty even with
            // zero state mutations).
            dirty: true,
            pull_progress: None,
            workflow_in_progress: None,
            workflow_cancel: None,
            doom_loop: None,
            doom_loop_selection: crate::tui::widgets::doom_banner::DoomLoopSelection::default(),
            completion_suggestions: Vec::new(),
            active_tab: ActiveTab::default(),
            tab_list_state: None,
            continuation: None,
            sessions_dirty: false,
            jobs_dirty: false,
            cached_jobs_output: None,
            #[cfg(unix)]
            daemon_flags: None,
            slash_menu: None,
            file_completer: None,
            cwd: std::env::current_dir().unwrap_or_default(),
        }
    }

    /// Should the tool entry at `idx` be collapsed to its summary line?
    /// True when collapse mode is on AND the user hasn't explicitly expanded it.
    #[inline]
    pub fn tool_should_collapse(&self, idx: usize) -> bool {
        // A streaming tool entry must stay expanded so the user can watch
        // the PTY output arrive; it collapses once `ToolResult` finalizes it.
        if self.messages.get(idx).map(|m| m.streaming).unwrap_or(false) {
            return false;
        }
        self.tool_collapsed && !self.expanded_tools.contains(&idx)
    }

    /// Has the user explicitly collapsed the message at `idx`?
    /// Non-tool messages are expanded by default.
    #[inline]
    pub fn message_should_collapse(&self, idx: usize) -> bool {
        self.collapsed_messages.contains(&idx)
    }

    /// Mark the state as needing a redraw. Cheap (single bool write);
    /// safe to call from any code path that mutates a field the
    /// renderer reads. The event loop clears the flag at the end of
    /// each render.
    #[inline]
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Compute (line_count, byte_count) for a tool output string,
    /// using the same line-wrapping width the chat renderer uses so the
    /// summary matches the visual height the user would see if expanded.
    pub fn tool_output_metrics(s: &str, wrap_width: usize) -> (usize, usize) {
        let width = wrap_width.max(1);
        let mut lines = 0usize;
        for segment in s.split('\n') {
            let len = segment.chars().count();
            // textwrap::fill would produce ceil(len/width) wrapped lines,
            // and an empty segment still occupies one line.
            lines += if len == 0 { 1 } else { len.div_ceil(width) };
        }
        (lines.max(1), s.len())
    }

    /// Return a spinner character based on the frame tick.
    pub fn spinner_char(&self) -> &'static str {
        const SPINNERS: &[&str] = &["▁", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄", "▃"];
        SPINNERS[(self.spinner_tick as usize) % SPINNERS.len()]
    }

    /// Convert the char-index cursor to a byte offset for string slicing.
    /// Returns `input.len()` if cursor is past the last character.
    #[inline]
    pub fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor_position)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    /// Number of logical lines in the input buffer (split on `\n`).
    /// Includes the empty trailing line created by a final newline so the
    /// user can keep typing after pressing Shift+Enter.
    #[inline]
    pub fn input_line_count(&self) -> usize {
        self.input.split('\n').count()
    }

    /// Return the cursor position as `(line, column)` char indices.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0usize;
        let mut col = 0usize;
        for (i, c) in self.input.chars().enumerate() {
            if i == self.cursor_position {
                return (line, col);
            }
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Visible height of the input box in terminal rows, including borders.
    /// Grows with the line count up to `max_rows`.
    pub fn input_visible_height(&self, max_rows: u16) -> u16 {
        let lines = self.input_line_count();
        lines.min(max_rows as usize).max(1) as u16 + 2
    }
}

/// A single entry in the conversation display.
#[derive(Debug, Clone)]
pub struct ConversationEntry {
    pub role: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Local>,
    /// Optional full tool output, stored only for `role == "tool"` entries.
    /// When `None`, the `content` field IS the full output (legacy/forward-compat).
    /// When `Some`, the UI may render `content` as a summary and expand
    /// via the stored `tool_output` on user request.
    pub tool_output: Option<String>,
    /// True while a tool is still running and streaming partial output
    /// into this entry (PTY path). The tool card renders a spinner and
    /// the accumulated `content` as incremental text until `ToolResult`
    /// finalizes the entry.
    pub streaming: bool,
    /// Render-generation counter. Bumped whenever `content` or `tool_output`
    /// changes so the chat render cache can validate with an O(1) integer
    /// comparison instead of hashing every byte of every message each frame.
    pub version: u64,
}

impl ConversationEntry {
    /// Construct a plain (non-tool) conversation entry.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: chrono::Local::now(),
            tool_output: None,
            streaming: false,
            version: 0,
        }
    }

    /// Construct a tool entry with full output stored separately.
    /// `summary` is what the chat shows when collapsed; `full` is shown
    /// when the user explicitly expands this entry.
    pub fn tool(summary: impl Into<String>, full: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: summary.into(),
            timestamp: chrono::Local::now(),
            tool_output: Some(full.into()),
            streaming: false,
            version: 0,
        }
    }

    /// Bump the render-generation counter after mutating content.
    #[inline]
    pub fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

/// State held while waiting for approval of a destructive tool call.
pub struct PendingApproval {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub responder: Option<crate::session::executor::ApprovalResponder>,
}

/// State held while waiting for approval of a `!` bang command.
///
/// The model-bash approval flow uses `PendingApproval` + a oneshot back to
/// the executor. The bang flow is local — no executor round trip — so it
/// gets its own field. The dialog renderer checks both; the key handler
/// branches on which is set.
///
/// Review.md (arch concern #1) flagged that the previous `!` handler
/// silently bypassed the approval flow even when `bang_requires_approval`
/// was on. This struct is the gate.
pub struct PendingBangCommand {
    pub cmd: String,
}

#[cfg(test)]
mod tests {
    use crate::shared::test_util::app_state;

    /// A freshly-constructed `AppState` must start with `dirty = true`
    /// so the first frame draws the connection banner / status bar
    /// even if no state mutation has happened yet. The render-on-
    /// state-change refactor (tui/mod.rs) relies on this initial
    /// dirty value; if it ever flips to `false`, the very first
    /// iteration of the event loop would skip `terminal.draw` and
    /// the user would see a blank screen until the slow-tick fired.
    #[test]
    fn new_state_starts_dirty() {
        let s = app_state();
        assert!(
            s.dirty,
            "freshly-constructed state should be dirty for the first frame"
        );
    }

    /// `mark_dirty` is a no-op when the state is already dirty, and
    /// idempotent across repeated calls. The cheap bool write is
    /// safe to call from any mutation site.
    #[test]
    fn mark_dirty_is_idempotent() {
        let mut s = app_state();
        s.dirty = false;
        s.mark_dirty();
        assert!(s.dirty);
        s.mark_dirty();
        assert!(s.dirty);
        // And reset path is just a bool write.
        s.dirty = false;
        assert!(!s.dirty);
    }
}
