/// Main application state and event handling.
use crate::session::session_fork::ForkManager;
use crate::session::skills::SkillRegistry;
use crate::shared::{ModelInfo, SharedConfig};
use crate::tui::theme::Theme;
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

/// Active overlay for the TUI panel system.
///
/// `None` is the default — chat-only mode, no overlay. F1–F6 (and the
/// Ctrl-shortcuts) summon an overlay on top of the chat surface; Esc
/// clears it back to `None`. The former persistent tab bar is gone
/// (WO 34.1); the command palette (Ctrl+K) is the discovery mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveTab {
    /// Chat-only mode — no overlay (default).
    #[default]
    None,
    /// F1 / Ctrl+M — Model / adapter info and switching
    Chat,
    /// F2 / Ctrl+M — Model / adapter info and switching
    Models,
    /// F3 / Ctrl+P — Plugin list, status, toggle
    Plugins,
    /// F4 / Ctrl+J — Scheduled and background job status
    Jobs,
    /// F5 / Ctrl+, — Config display and live reload
    Settings,
    /// F6 / Ctrl+S — Threads overview (forks + sessions)
    Threads,
}

impl ActiveTab {
    /// All overlay tabs in F-key order (excludes `None`).
    /// Used by the mouse handler and selftest; the tab bar itself is gone.
    pub const OVERLAYS: [ActiveTab; 6] = [
        ActiveTab::Chat,
        ActiveTab::Models,
        ActiveTab::Plugins,
        ActiveTab::Jobs,
        ActiveTab::Settings,
        ActiveTab::Threads,
    ];

    /// Short label for overlays (e.g. "Models"). Returns "" for `None`.
    /// Matches the overlay panel header text, not the command-palette
    /// action label (the palette action "Open sessions" maps to
    /// `ActiveTab::Threads`; this label is "Threads" to match the panel).
    pub fn label(&self) -> &'static str {
        match self {
            ActiveTab::None => "",
            ActiveTab::Chat => "Chat",
            ActiveTab::Models => "Models",
            ActiveTab::Plugins => "Plugins",
            ActiveTab::Jobs => "Jobs",
            ActiveTab::Settings => "Settings",
            ActiveTab::Threads => "Threads",
        }
    }

    /// Map an F-key code to a tab, or return None for non-F keys.
    /// F-keys are the invisible muscle-memory fallback (no tab bar shown).
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

/// Conversation view state (WO 26.8).
pub struct ConversationState {
    /// Conversation messages
    pub messages: VecDeque<ConversationEntry>,
    /// Current user input buffer
    pub input: String,
    /// Cursor position as a Unicode **char index** (not byte offset).
    /// This is safe across UTF-8 multi-byte characters. Convert to byte
    /// offset via [`AppState::cursor_byte`] before any string slicing.
    pub cursor_position: usize,
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
    // ── Code-block copy cycle (P3) ────────────────────────────────
    /// `Ctrl+Shift+B` cycles through the code blocks of the most
    /// recent assistant message. This counter tracks which block is
    /// copied next; it wraps around when it reaches the number of
    /// blocks in that message.
    pub code_block_copy_index: usize,
    // ── Tab-completion suggestions (WO 14.6) ───────────────────────
    /// One-line completion list shown above/below the input when Tab
    /// produces multiple matches (slash commands or @-mention paths).
    /// Empty when there is nothing to suggest. The key handler fills
    /// it on Tab; any other keypress clears it. Rendered as a dim
    /// hint line in `widgets/input.rs`.
    pub completion_suggestions: Vec<String>,
}

impl Default for ConversationState {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
            input: String::new(),
            cursor_position: 0,
            scroll_offset: 0,
            auto_scroll: true,
            max_scroll: 0,
            tool_collapsed: true,
            expanded_tools: HashSet::new(),
            chat_render_cache: ChatRenderCache::default(),
            last_content_width: 0,
            collapsed_messages: HashSet::new(),
            code_block_copy_index: 0,
            completion_suggestions: Vec::new(),
        }
    }
}

/// Generation / background-run state (WO 26.8).
#[derive(Default)]
pub struct GenerationState {
    /// Thinking panel (collapsible)
    pub thinking_panel_visible: bool,
    pub thinking_buffer: Vec<String>,
    /// True while the model is generating a response (between Enter and Done).
    pub is_generating: bool,
    /// Tool calls made so far in the current executor turn. Reset when a
    /// turn completes (CostStats). Shown in the status bar so the user
    /// can see progress even when is_generating is false between tool calls.
    pub turn_tool_calls: usize,
    /// Spinner frame counter — cycles through a spinner animation
    /// to show the model is thinking before the first token arrives.
    pub spinner_tick: u64,
    /// Continuation round indicator (WO 23.9-R3). When `Some((round, max))`,
    /// the executor is in a `FinishReason::Length` continuation loop. The
    /// status bar renders "⟳ round/max" in Yellow. Cleared when the turn
    /// completes normally (CostStats).
    pub continuation: Option<(usize, usize)>,
    /// Fork-isolated subagent currently running in the background.
    pub persona_in_progress: Option<crate::tui::commands::PersonaHandle>,
    /// Cancel flag for the running persona, checked between internal turns.
    pub persona_cancel: Option<Arc<AtomicBool>>,
    /// Handle to the workflow currently running in the background.
    pub workflow_in_progress: Option<crate::tui::commands::WorkflowHandle>,
    /// Cancel flag for the running workflow, checked between steps.
    pub workflow_cancel: Option<Arc<AtomicBool>>,
    /// True while a `/test` command is running. Used to (1) gate the
    /// input box against stacking tests, (2) drive the spinner in
    /// place of the model-generation spinner.
    pub test_in_progress: bool,
}

/// Token / cost / cache budget state (WO 26.8).
pub struct BudgetState {
    /// Token counters
    pub tokens_sent: usize,
    pub tokens_received: usize,
    /// Cost tracking
    pub turn_cost: f64,
    pub cumulative_cost: f64,
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
}

impl Default for BudgetState {
    fn default() -> Self {
        Self {
            tokens_sent: 0,
            tokens_received: 0,
            turn_cost: 0.0,
            cumulative_cost: 0.0,
            last_turn_prompt_tokens: 0,
            cached_tokens: 0,
            stem_tokens: 0,
            cache_hit_ratio: 0.0,
        }
    }
}

/// Session lifecycle / persistence / daemon-push state (WO 26.8).
pub struct SessionState {
    /// Session start time
    pub session_started: Instant,
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
    /// Set of background job IDs that have already been notified as completed.
    /// Used to avoid repeated notifications for the same job.
    pub notified_jobs: std::collections::HashSet<u64>,
    /// Set of scheduled-job run IDs that have already been notified as
    /// completed. Each scheduled job overwrites its `last_run`, so this tracks
    /// run IDs (not job IDs) so every run is announced exactly once.
    pub notified_scheduled_runs: std::collections::HashSet<String>,
    /// Recent-session picker (daemon follow-up). When set, the TUI is showing
    /// the recent-session picker overlay instead of the normal input box.
    pub session_picker: Option<crate::tui::components::session_picker::SessionPicker>,
    /// Shared undo stack (review.md gap #7). The executor owns the write side;
    /// the TUI uses it read-only for `/undo list` and `/undo count`.
    pub undo_stack: Option<crate::tools::UndoStackRef>,
    /// Memory visibility widget (WO 26.7-R3). Latest memory store size and
    /// the executor turn that last mutated it.
    pub memory_status: Option<(usize, u64)>,
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
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            session_started: Instant::now(),
            log_path: None,
            session_id: String::new(),
            fork_manager: None,
            should_exit: false,
            notified_jobs: std::collections::HashSet::new(),
            notified_scheduled_runs: std::collections::HashSet::new(),
            session_picker: None,
            undo_stack: None,
            memory_status: None,
            sessions_dirty: false,
            jobs_dirty: false,
            cached_jobs_output: None,
            #[cfg(unix)]
            daemon_flags: None,
        }
    }
}

/// Provider / connection / plugin state (WO 26.8).
pub struct ProviderState {
    /// Connection
    pub connection: ConnectionState,
    pub model_info: Option<ModelInfo>,
    /// Ollama pull progress (gap #22). Latest pull-progress event received
    /// from `/api/pull`. Used by the renderer to draw a progress bar in the
    /// chat panel. `None` when no pull is in progress.
    pub pull_progress: Option<PullProgress>,
    /// PathGuard sandbox indicator (v1.2-p12 follow-up). If true, the session
    /// is intentionally unsandboxed. The TUI chat banner and status bar
    /// surface this so the operator sees the posture.
    pub unsandboxed: bool,
    /// Plugin trust-tier status (Phase 2.3). Compact summary of loaded plugin
    /// trust tiers, displayed in the status bar. `None` when no plugins.
    pub plugin_status: Option<String>,
    /// Runtime plugin registry (Phase 11). In-TUI copy of the active plugin
    /// registry. Mutated by `/plugins` commands and forwarded to the executor.
    pub plugin_registry: PluginRegistry,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Disconnected,
            model_info: None,
            pull_progress: None,
            unsandboxed: false,
            plugin_status: None,
            plugin_registry: PluginRegistry::new(),
        }
    }
}

/// Approval-dialog state (v1.2-p11) (WO 26.8).
#[derive(Default)]
pub struct ApprovalState {
    /// Tool call status
    pub pending_approval: Option<PendingApproval>,
    /// Bang approval gate (review.md arch concern #1). When `Some`, the user
    /// has typed `!` with `bang_requires_approval` enabled. Mirrors
    /// `pending_approval` in shape but doesn't go through the executor's
    /// oneshot channel — bang is a pure local feature.
    pub pending_bang: Option<PendingBangCommand>,
    /// Vertical scroll offset into the args preview, in lines. 0 = top of args.
    /// Set by the approval-mode key handler; reset to 0 in
    /// `drain_approval_requests` whenever a new approval arrives.
    pub approval_scroll: usize,
    /// Max valid scroll offset for the current approval's args preview. Set
    /// each render in `render_approval_dialog`.
    pub approval_max_scroll: usize,
    /// Toggle between unified diff and side-by-side diff in the approval dialog.
    pub approval_diff_side_by_side: bool,
}

/// Tab UI state (WO 26.8).
pub struct UiState {
    /// Slash-command popup. When `Some`, a filterable popup listing slash
    /// commands is shown above the input bar.
    pub slash_menu: Option<SlashMenu>,
    /// @-mention file completer popup. When `Some`, a directory-browsing
    /// popup for @-mentions is shown above the input bar.
    pub file_completer: Option<FileCompleter>,
    /// Tab panel system. Currently active tab. F1–F6 switch tabs.
    pub active_tab: ActiveTab,
    /// Row selection state for the active tab panel (Models, Plugins, Jobs,
    /// Settings, Threads). `None` for Chat (no selectable rows).
    pub tab_list_state: Option<usize>,
    /// Current working directory. Updated by Ctrl+O directory picker.
    pub cwd: std::path::PathBuf,
    /// Active color palette (WO 27.6). Seeded from `display.theme` at
    /// startup; mutated by the `/theme` slash command. Renderers in
    /// `src/tui/rendering/` read colors from this field via `&Theme`.
    pub theme: Theme,
    /// Last row seen during an in-progress left-button drag, used by
    /// `events::handle_mouse_event` to drag-scroll the chat by the
    /// row delta. `None` when no drag is active (cleared on
    /// `MouseEventKind::Up`) (WO 27.7).
    pub mouse_drag_row: Option<u16>,
    /// Brief render-countdown after a bracketed paste. The input title shows
    /// "📋 pasted" while this is > 0. Set on `Event::Paste`, decremented once
    /// per slow-tick (125 ms), and cleared on the next keystroke. A `u8`
    /// countdown instead of a `bool` so the indicator can fade on its own
    /// without a second field (WO 30.0.11).
    pub paste_flash: u8,
    /// Last-rendered input-box rect, set by `render_app` each frame so the
    /// mouse handler can hit-test clicks against the input area (WO 32.12).
    /// `None` until the first render completes.
    pub last_input_rect: Option<ratatui::layout::Rect>,
    /// Command palette (Ctrl+K). When `true`, the centered overlay is
    /// shown with a search input + filtered action list. Typing filters
    /// (fuzzy match), ↑↓ navigates, Enter activates, Esc closes.
    pub command_palette_visible: bool,
    /// Current search query in the command palette.
    pub command_palette_query: String,
    /// Highlighted row in the command palette's filtered list.
    pub command_palette_selected: usize,
    /// `/help` overlay visibility (WO 34.2). When true, `render_app` draws
    /// `help_overlay` on top of the chat. The `/help` slash command sets
    /// this instead of pushing help text into the conversation; Esc clears
    /// it. Keeps the conversation + session log free of help text.
    pub help_overlay_visible: bool,
    /// Scroll offset (in lines) of the help overlay. ↑/↓ adjust; Esc
    /// resets to 0 when the overlay closes.
    pub help_overlay_scroll: usize,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            slash_menu: None,
            file_completer: None,
            active_tab: ActiveTab::default(),
            tab_list_state: None,
            cwd: std::env::current_dir().unwrap_or_default(),
            theme: Theme::default(),
            mouse_drag_row: None,
            paste_flash: 0,
            last_input_rect: None,
            command_palette_visible: false,
            command_palette_query: String::new(),
            command_palette_selected: 0,
            help_overlay_visible: false,
            help_overlay_scroll: 0,
        }
    }
}

/// Doom-loop detection banner state (WO 8.2) (WO 26.8).
#[derive(Default)]
pub struct DoomLoopUiState {
    /// Set when the executor reports a doom loop (same tool failing the same
    /// way N turns in a row).
    pub doom_loop: Option<DoomLoopState>,
    /// Banner highlight position. Independent of `DoomLoopState` so the user
    /// can move the highlight before committing an action.
    pub doom_loop_selection: crate::tui::widgets::doom_banner::DoomLoopSelection,
}

/// Long-lived service handles (WO 26.8).
pub struct ServicesState {
    /// Shared config reference. Kept behind an `Arc<RwLock>` so that
    /// SIGHUP/`/reload` can update live behavior without restarting.
    pub config: SharedConfig,
    /// Skill registry for slash commands (loaded from SKILL.md files)
    pub skill_registry: SkillRegistry,
}

impl ServicesState {
    fn new(config: SharedConfig) -> Self {
        Self {
            config,
            skill_registry: SkillRegistry::new(),
        }
    }
}

/// Application state — single source of truth for the TUI.
///
/// Decomposed from a single flat ~66-field struct into ≤12 sub-structs
/// grouped by concern (WO 26.8). Call sites access fields through the owning
/// sub-struct (e.g. `state.conversation.messages`). The small set of
/// cross-cutting helper methods (`mark_dirty`, `cursor_byte`,
/// `tool_should_collapse`, `message_should_collapse`, `spinner_char`,
/// `input_*`) remain on `AppState` and serve as accessor shims so the TUI
/// render path does not need to reach into sub-structs for shared logic.
pub struct AppState {
    /// Conversation view state
    pub conversation: ConversationState,
    /// Generation / background-run state
    pub generation: GenerationState,
    /// Token / cost / cache budget state
    pub budget: BudgetState,
    /// Session lifecycle / persistence / daemon-push state
    pub session: SessionState,
    /// Provider / connection / plugin state
    pub provider: ProviderState,
    /// Approval-dialog state
    pub approval: ApprovalState,
    /// Conversation search (review.md gap #4)
    pub search: SearchState,
    /// Tab / popup UI state
    pub ui: UiState,
    /// Doom-loop detection banner state
    pub doom: DoomLoopUiState,
    /// Long-lived service handles
    pub services: ServicesState,
    /// Frame-pacing v2: render-on-state-change flag. Set to `true` whenever
    /// `state` mutates in a way that should produce a redraw. The event loop
    /// checks this flag at the top of each iteration and skips `terminal.draw`
    /// when it's still `false`.
    pub dirty: bool,
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
            conversation: ConversationState::default(),
            generation: GenerationState::default(),
            budget: BudgetState::default(),
            session: SessionState::default(),
            provider: ProviderState::default(),
            approval: ApprovalState::default(),
            search: SearchState::default(),
            ui: UiState::default(),
            doom: DoomLoopUiState::default(),
            services: ServicesState::new(config),
            // Start dirty so the first frame draws immediately (the
            // connection banner / status bar are non-empty even with
            // zero state mutations).
            dirty: true,
        }
    }

    /// Should the tool entry at `idx` be collapsed to its summary line?
    /// True when collapse mode is on AND the user hasn't explicitly expanded it.
    #[inline]
    pub fn tool_should_collapse(&self, idx: usize) -> bool {
        // A streaming tool entry must stay expanded so the user can watch
        // the PTY output arrive; it collapses once `ToolResult` finalizes it.
        if self
            .conversation
            .messages
            .get(idx)
            .map(|m| m.streaming)
            .unwrap_or(false)
        {
            return false;
        }
        self.conversation.tool_collapsed && !self.conversation.expanded_tools.contains(&idx)
    }

    /// Has the user explicitly collapsed the message at `idx`?
    /// Non-tool messages are expanded by default.
    #[inline]
    pub fn message_should_collapse(&self, idx: usize) -> bool {
        self.conversation.collapsed_messages.contains(&idx)
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
        SPINNERS[(self.generation.spinner_tick as usize) % SPINNERS.len()]
    }

    /// Convert the char-index cursor to a byte offset for string slicing.
    /// Returns `input.len()` if cursor is past the last character.
    #[inline]
    pub fn cursor_byte(&self) -> usize {
        self.conversation
            .input
            .char_indices()
            .nth(self.conversation.cursor_position)
            .map(|(b, _)| b)
            .unwrap_or(self.conversation.input.len())
    }

    /// Number of VISUAL rows the input occupies at `content_width` char
    /// columns. Each logical line wraps to `ceil(chars / content_width)`
    /// rows (minimum 1), and the per-line counts are summed. This is what
    /// the input box must grow to so a long paste / long line stays visible
    /// (WO 30.0.12). `content_width` is clamped to ≥ 1.
    pub fn input_visual_line_count(&self, content_width: usize) -> usize {
        let width = content_width.max(1);
        self.conversation
            .input
            .split('\n')
            .map(|line| {
                let chars = line.chars().count();
                if chars == 0 {
                    1
                } else {
                    chars.div_ceil(width)
                }
            })
            .sum()
    }

    /// Insert `text` at the cursor and advance the cursor by its char count.
    /// Used by bracketed-paste handling (WO 30.0.11).
    pub fn apply_paste(&mut self, text: &str) {
        let byte_pos = self.cursor_byte();
        self.conversation.input.insert_str(byte_pos, text);
        self.conversation.cursor_position += text.chars().count();
    }

    /// Return the cursor position as `(line, column)` char indices.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0usize;
        let mut col = 0usize;
        for (i, c) in self.conversation.input.chars().enumerate() {
            if i == self.conversation.cursor_position {
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
    /// Grows with the VISUAL line count (wraps long lines at `content_width`)
    /// up to `max_rows` (WO 30.0.12).
    pub fn input_visible_height(&self, max_rows: u16, content_width: usize) -> u16 {
        let lines = self.input_visual_line_count(content_width);
        lines.min(max_rows as usize).max(1) as u16 + 2
    }

    /// Move the text cursor to the character at `(line, col)` char indices,
    /// clamping to the input buffer bounds. Used by the click-in-prompt
    /// handler (WO 32.12). `line` and `col` are logical (not visual/wrapped)
    /// char indices into the input string.
    pub fn set_cursor_line_col(&mut self, line: usize, col: usize) {
        let input = &self.conversation.input;
        let mut current_line = 0usize;
        for (char_idx, c) in input.char_indices() {
            if current_line == line {
                let line_rest: String = input[char_idx..]
                    .chars()
                    .take_while(|c2| *c2 != '\n')
                    .collect();
                self.conversation.cursor_position = char_idx + col.min(line_rest.chars().count());
                return;
            }
            if c == '\n' {
                current_line += 1;
            }
        }
        self.conversation.cursor_position = input.chars().count();
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

    // ── WO 30.0.12: visual-wrap line count ──────────────────────────
    //
    // `input_visual_line_count` must count VISUAL rows (wrapping long
    // lines at the content width), not just `\n`-separated logical lines.
    // A 300-char single line at width 60 wraps to 5 rows; a bug that
    // returns 1 here is exactly the 30.0.12 regression (input box stays
    // at minimum height and clips the wrapped text).

    #[test]
    fn visual_line_count_wraps_long_line() {
        let mut s = app_state();
        s.conversation.input = "x".repeat(300);
        // 300 chars / 60 width = 5 rows.
        assert_eq!(s.input_visual_line_count(60), 5);
        // content_width=0 clamps to 1 → 300 rows (no divide-by-zero).
        assert_eq!(s.input_visual_line_count(0), 300);
    }

    #[test]
    fn visual_line_count_sums_across_newlines() {
        let mut s = app_state();
        // 60-char line (1 row at width 60) + empty line (1 row) + 90-char
        // line (2 rows) = 4 visual rows.
        s.conversation.input = format!("{}\n\n{}", "a".repeat(60), "b".repeat(90));
        assert_eq!(s.input_visual_line_count(60), 4);
    }

    #[test]
    fn input_visible_height_grows_with_visual_wrap() {
        let mut s = app_state();
        s.conversation.input = "y".repeat(300);
        // 5 visual rows clamped to max_rows=5 → 5 + 2 borders = 7.
        assert_eq!(s.input_visible_height(5, 60), 7);
        // 300 visual rows (width 1) clamp to max_rows=5 → still 7.
        assert_eq!(s.input_visible_height(5, 1), 7);
        // Empty input → 1 row + 2 borders = 3.
        s.conversation.input.clear();
        assert_eq!(s.input_visible_height(5, 60), 3);
    }

    // ── WO 30.0.11: paste inserts at cursor + advances it ───────────

    #[test]
    fn apply_paste_inserts_at_cursor_and_advances() {
        let mut s = app_state();
        s.conversation.input = "hello world".to_string();
        // Cursor between "hello" and " world" (char index 5).
        s.conversation.cursor_position = 5;
        s.apply_paste(" brave");
        assert_eq!(s.conversation.input, "hello brave world");
        // char count of " brave" = 6 → cursor advanced to 11.
        assert_eq!(s.conversation.cursor_position, 11);
    }

    #[test]
    fn apply_paste_handles_multibyte_at_cursor() {
        let mut s = app_state();
        s.conversation.input = "héllo".to_string(); // é is one char, two bytes
        s.conversation.cursor_position = 1; // after 'h'
        s.apply_paste("X");
        assert_eq!(s.conversation.input, "hXéllo");
        assert_eq!(s.conversation.cursor_position, 2);
    }
}
