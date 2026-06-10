/// Main application state and event handling.
use crate::session::session_fork::ForkManager;
use crate::session::skills::SkillRegistry;
use crate::shared::{Config, ModelInfo};
use std::path::PathBuf;
use std::time::Instant;

/// Represents the connection state for the status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected { model: String, since: Instant },
    Error(String),
}

/// Application state — single source of truth for the TUI.
pub struct AppState {
    /// Conversation messages
    pub messages: Vec<ConversationEntry>,

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

    /// Config reference
    pub config: Config,

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

    /// Spinner frame counter — cycles through a spinner animation
    /// to show the model is thinking before the first token arrives.
    pub spinner_tick: u64,

    /// Set of background job IDs that have already been notified as completed.
    /// Used to avoid repeated notifications for the same job.
    pub notified_jobs: std::collections::HashSet<u64>,

    // ── Tool output collapse (v1.1) ───────────────────────────────
    /// When true, long tool entries are collapsed to a one-line summary.
    /// Toggled with Ctrl+T. Default true so the chat view is never flooded
    /// by default — users opt in to the full flood.
    pub tool_collapsed: bool,

    /// Per-index expansion override: even when `tool_collapsed` is true,
    /// an entry whose index is in this set renders in full. Allows users
    /// to expand specific tool results they want to inspect.
    pub expanded_tools: std::collections::HashSet<usize>,

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

    // ── Bang approval gate (review.md arch concern #1) ─────────────
    /// When `Some`, the user has typed `!` with `bang_requires_approval`
    /// enabled, and is being shown the approval dialog for the local
    /// (no-model) bash run. `None` in the common case. Mirrors
    /// `pending_approval` in shape but doesn't go through the executor's
    /// oneshot channel — bang is a pure local feature.
    pub pending_bang: Option<PendingBangCommand>,

    // ── Conversation search (review.md gap #4) ─────────────
    /// When `true`, the input box is being used as a search bar.
    /// Ctrl+F enters search mode; typing filters the chat
    /// conversation; Enter commits and leaves the matches
    /// highlighted; Esc cancels and clears the matches.
    pub search_mode: bool,
    /// The current search query (built up while in search mode).
    /// Empty when not searching.
    pub search_query: String,
    /// All match positions in the conversation, in document order.
    /// Each entry is `(message_index, byte_offset)` for the start
    /// of the match in `messages[message_index].content`. Filled in
    /// when search is committed; cleared on cancel or `/clear`.
    pub search_matches: Vec<(usize, usize)>,
    /// Index into `search_matches` of the currently-highlighted
    /// match. `n` cycles forward, `N` (Shift+N) cycles backward.
    /// When `search_matches.is_empty()`, this is meaningless.
    pub search_match_idx: usize,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            messages: Vec::new(),
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
            spinner_tick: 0,
            notified_jobs: std::collections::HashSet::new(),
            tool_collapsed: true,
            expanded_tools: std::collections::HashSet::new(),
            approval_scroll: 0,
            approval_max_scroll: 0,
            last_turn_prompt_tokens: 0,
            pending_bang: None,
            search_mode: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        }
    }

    /// Should the tool entry at `idx` be collapsed to its summary line?
    /// True when collapse mode is on AND the user hasn't explicitly expanded it.
    #[inline]
    pub fn tool_should_collapse(&self, idx: usize) -> bool {
        self.tool_collapsed && !self.expanded_tools.contains(&idx)
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
}

impl ConversationEntry {
    /// Construct a plain (non-tool) conversation entry.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: chrono::Local::now(),
            tool_output: None,
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
        }
    }
}

/// State held while waiting for approval of a destructive tool call.
pub struct PendingApproval {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub responder: Option<tokio::sync::oneshot::Sender<crate::session::executor::ApprovalResponse>>,
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
