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
        }
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
}

/// State held while waiting for approval of a destructive tool call.
pub struct PendingApproval {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub responder: Option<tokio::sync::oneshot::Sender<crate::session::executor::ApprovalResponse>>,
}
