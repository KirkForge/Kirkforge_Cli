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
    pub cursor_position: usize,

    /// Connection
    pub connection: ConnectionState,
    pub model_info: Option<ModelInfo>,

    /// Scroll position for the chat view
    pub scroll_offset: usize,

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
        }
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
