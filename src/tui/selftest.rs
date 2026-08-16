//! TUI selftest harness — drives the FULL render pipeline against an
//! in-memory ratatui `Buffer` (no terminal / PTY / tmux required).
//!
//! This module is the WO 31.6 answer to "TUI render bugs found manually":
//! it feeds realistic `TurnEvent` sequences into an `AppState`, renders
//! via the SAME `render_app` function the production event loop uses
//! (`tui::mod::render_app`, extracted from `render_frame`'s closure for
//! exactly this purpose), and asserts on the resulting buffer text.
//!
//! Runs in well under a second as `cargo test --lib tui::selftest`. Each
//! scenario targets a specific bug class (overflow, panic, missing content,
//! word-wrap regression). Adding a new regression test = copy a scenario,
//! feed the events that triggered it, assert the fix.

use crate::session::executor::TurnEvent;
use crate::shared::test_util::app_state;
use crate::tui::app::{
    ActiveTab, AppState, ConnectionState, ConversationEntry, DoomLoopState, PendingApproval,
    SlashMenu,
};
use crate::tui::events::dispatch_turn_event;
use ratatui::{backend::TestBackend, Terminal};

/// Default harness geometry — a typical terminal. Wide/tall enough that
/// none of the panel layouts collapse to degenerate widths.
const DEFAULT_WIDTH: u16 = 120;
const DEFAULT_HEIGHT: u16 = 40;

/// Render the full TUI pipeline into a string.
///
/// Builds a `TestBackend(width, height)`, drives `render_app` (the exact
/// function `render_frame` calls in production), then flattens the
/// resulting `Buffer` to printable text: one line per terminal row,
/// grapheme cells joined left-to-right, trailing whitespace trimmed.
///
/// Returned text is suitable for `str::contains` assertions. Pixel-perfect
/// layout claims are deliberately avoided — the harness exists to catch
/// panics, overflows, and missing content, not to pin glyph coordinates.
pub fn render_to_string(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend build");
    terminal
        .draw(|f| super::render_app(f, state))
        .expect("render_app must not panic");
    buffer_to_string(terminal.backend().buffer())
}

fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
    let mut rows = Vec::with_capacity(buffer.area.height as usize);
    for y in 0..buffer.area.height {
        let mut row = String::with_capacity(buffer.area.width as usize);
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                row.push_str(cell.symbol());
            } else {
                row.push(' ');
            }
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

/// Test harness: owns an `AppState` and exposes a small fluent API for
/// feeding events and asserting on the rendered output.
///
/// Mirrors how the real TUI mutates state: `feed_event` calls the same
/// `dispatch_turn_event` the event loop calls, so the state transitions
/// under test are byte-for-byte identical to production.
pub struct TuiTestHarness {
    pub state: AppState,
    pub width: u16,
    pub height: u16,
}

impl TuiTestHarness {
    /// Fresh harness with a default-config `AppState` and 120×40 geometry.
    pub fn new() -> Self {
        Self {
            state: app_state(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        }
    }

    /// Mark the harness as connected to a model so the chat panel does
    /// not render the "Disconnected" banner (cleans up assertions on
    /// chat content).
    pub fn connected(mut self, model: &str) -> Self {
        self.state.provider.connection = ConnectionState::Connected {
            model: model.to_string(),
            since: std::time::Instant::now(),
        };
        self
    }

    /// Apply a single `TurnEvent` via the production dispatch path.
    pub fn feed_event(&mut self, ev: TurnEvent) {
        dispatch_turn_event(&mut self.state, ev);
    }

    /// Apply a sequence of events in order.
    pub fn feed_events(&mut self, events: impl IntoIterator<Item = TurnEvent>) {
        for ev in events {
            dispatch_turn_event(&mut self.state, ev);
        }
    }

    /// Render the full TUI to a string at the harness geometry.
    pub fn render(&mut self) -> String {
        render_to_string(&mut self.state, self.width, self.height)
    }

    /// Render and assert the output contains `needle` (panics with a
    /// rendered-buffer dump on failure for fast diagnosis).
    pub fn assert_contains(&mut self, needle: &str) {
        let rendered = self.render();
        assert!(
            rendered.contains(needle),
            "expected rendered TUI to contain {needle:?}\n\
             ===== rendered buffer =====\n{rendered}\n============================"
        );
    }

    /// Render and assert the output does NOT contain `needle`.
    pub fn assert_not_contains(&mut self, needle: &str) {
        let rendered = self.render();
        assert!(
            !rendered.contains(needle),
            "expected rendered TUI to NOT contain {needle:?}\n\
             ===== rendered buffer =====\n{rendered}\n============================"
        );
    }
}

impl Default for TuiTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Scenarios. Each targets one bug class; comments name the class.
// ─────────────────────────────────────────────────────────────────────

/// Stress: 500 streaming tokens must accumulate into one assistant entry
/// without overflow, panic, or dropped content. Catches off-by-N in the
/// chat render cache and any width-dependent wrap panic on long content.
#[test]
fn token_stream_stress() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");

    // 500 distinct tokens so we can verify the data survived in state.
    for i in 0..500u32 {
        h.feed_event(TurnEvent::Token(format!("word{i} ")));
    }

    // The stream produced exactly one growing assistant entry, all 500
    // tokens accumulated (no drops), and the entry content ends with the
    // last token. This is the data-integrity contract.
    assert_eq!(h.state.conversation.messages.len(), 1);
    let msg = &h.state.conversation.messages[0];
    assert_eq!(msg.role, "assistant");
    assert!(
        msg.content.ends_with("word499 "),
        "last token must survive in the assistant entry content"
    );

    // The full render pipeline must complete without panicking or
    // overflowing the buffer (render() would panic on overflow).
    // WO 30.0.13: streaming content is rendered as plain text via
    // textwrap (not markdown), which pre-wraps into one Line per visual
    // row. This makes `max_scroll` correctly reflect the wrapped height,
    // so `auto_scroll` pins to the bottom and the latest tokens (word499)
    // are visible — the previous auto_scroll-on-long-message bug is fixed
    // for the streaming case. The ASSISTANT header scrolls off the top on
    // long content, which is the correct auto-scroll-to-bottom behaviour.
    h.assert_contains("word499");
}

/// Word-wrap regression guard for the thinking panel. The bug
/// (commit b6fe023) was one-word-per-line because each streaming
/// token was wrapped individually instead of joined first. This
/// scenario feeds 2000 chars of multi-word thinking text, toggles
/// the panel visible, and verifies (a) the THINKING header is
/// present and (b) at least one rendered row holds multiple
/// words — which one-word-per-line can never produce.
#[test]
fn thinking_block_wordwrap() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    // Seed an assistant message so the thinking block has a host to
    // attach under (render_chat attaches thinking to the last message).
    h.feed_event(TurnEvent::Token("answer".into()));
    // 2000 chars of multi-word prose. Each "sentence " is 10 chars;
    // 200 of them = 2000 chars, far wider than any terminal row.
    let prose = "sentence ".repeat(200);
    h.feed_event(TurnEvent::Thinking(prose));
    h.state.generation.thinking_panel_visible = true;

    let rendered = h.render();
    assert!(rendered.contains("THINKING"), "THINKING header missing");

    // Word-wrap proof: at least one row in the thinking block must
    // contain two space-separated words. One-word-per-line rendering
    // (the bug) produces no such row.
    let multi_word_row = rendered.lines().any(|line| {
        line.trim_start().starts_with("│") && {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.len() >= 2
        }
    });
    assert!(
        multi_word_row,
        "thinking text should wrap multiple words per row (word-wrap regression):\n{rendered}"
    );
}

/// Tool-call card render: `ToolStart` then `ToolResult` must both leave
/// a visible tool card with the tool name. Catches the card-collapse
/// state desync where a streaming tool entry never finalises.
#[test]
fn tool_call_card_render() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    // `feed_events` exercises the batch API (spec R1) — the two events
    // represent one logical tool turn (start → finalize).
    h.feed_events([
        TurnEvent::ToolStart {
            name: "bash".into(),
            args: serde_json::json!({"command": "echo hi"}),
        },
        TurnEvent::ToolResult {
            name: "bash".into(),
            output: "hi\n".into(),
            success: true,
        },
    ]);

    let rendered = h.render();
    // The finalized tool card carries the tool name in its summary line.
    assert!(
        rendered.contains("bash"),
        "tool name missing from rendered card"
    );
    // Two events collapse to ONE tool entry (ToolResult pops the
    // streaming placeholder), so exactly one tool message exists.
    assert_eq!(h.state.conversation.messages.len(), 1);
    assert_eq!(h.state.conversation.messages[0].role, "tool");
}

/// Tool call grouping (WO 30.0.14): consecutive non-streaming tool entries
/// collapse into a single "🔧 name ×N" header in the production render
/// path (`render_chat`). Catches the regression where grouping lived only
/// in `build_chat_lines` (search-scroll) and never appeared in the TUI.
/// Also verifies the three edge cases: single tool not grouped, streaming
/// PTY tool not grouped, and the expanded-group path rendering every
/// member individually.
#[test]
fn tool_call_grouping() {
    // ── 1. Multiple consecutive tools collapse to one header ──
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    for i in 0..3u32 {
        h.feed_events([
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"command": format!("echo {i}")}),
            },
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: format!("{i}\n"),
                success: true,
            },
        ]);
    }
    // Three finalized tool entries, all consecutive.
    assert_eq!(h.state.conversation.messages.len(), 3);
    assert!(h
        .state
        .conversation
        .messages
        .iter()
        .all(|m| m.role == "tool"));
    // tool_collapsed defaults to true → the group renders as one header.
    let rendered = h.render();
    assert!(
        rendered.contains("🔧"),
        "grouped tool header missing from production render"
    );
    assert!(
        rendered.contains("bash ×3"),
        "three consecutive bash calls must group into 'bash ×3'"
    );

    // ── 2. Single tool call is NOT grouped ──
    let mut h2 = TuiTestHarness::new().connected("qwen2.5");
    h2.feed_events([
        TurnEvent::ToolStart {
            name: "grep".into(),
            args: serde_json::json!({"pattern": "x"}),
        },
        TurnEvent::ToolResult {
            name: "grep".into(),
            output: "match\n".into(),
            success: true,
        },
    ]);
    let rendered2 = h2.render();
    // A single tool renders its own card with the "(done)" summary —
    // not a grouped header. (The status bar's "🔧×1" counter is
    // unrelated; we assert on the chat-specific "(done)" marker.)
    assert!(
        rendered2.contains("grep (done)"),
        "single tool card must show its own summary, not a group header"
    );
    assert!(
        !rendered2.contains("grep ×1"),
        "a single tool call must not be grouped"
    );

    // ── 3. Expanding any member renders all individually ──
    let mut h3 = TuiTestHarness::new().connected("qwen2.5");
    for i in 0..3u32 {
        h3.feed_events([
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"command": format!("echo {i}")}),
            },
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: format!("out{i}\n"),
                success: true,
            },
        ]);
    }
    // Expand the middle tool → the group must un-group so that tool's
    // body is visible. This also catches the idx-advance bug where the
    // expanded-mode fall-through skipped middle entries.
    h3.state.conversation.expanded_tools.insert(1);
    let rendered3 = h3.render();
    assert!(
        !rendered3.contains("bash ×3"),
        "expanding one member must un-group the block"
    );
    assert!(
        rendered3.contains("out1"),
        "expanded middle tool's body must be visible"
    );
}

/// Approval prompt display: a pending tool approval must render the
/// dialog with the tool name and the "Approval Required" header.
/// Catches the borrow-scope regression where `mem::take` of the
/// pending approval left the dialog blank.
#[test]
fn approval_prompt_display() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    h.state.approval.pending_approval = Some(PendingApproval {
        tool_name: "edit_file".into(),
        args: serde_json::json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"}),
        responder: None,
    });

    let rendered = h.render();
    assert!(
        rendered.contains("Approval Required"),
        "approval dialog header missing"
    );
    assert!(
        rendered.contains("edit_file"),
        "tool name missing from approval dialog"
    );
    // Pending approval survives the render (mem::take restores it).
    assert!(h.state.approval.pending_approval.is_some());
}

/// Budget indicator update: a `CostStats` event sets
/// `last_turn_prompt_tokens`; with a connected model_info carrying a
/// non-zero `max_context_tokens`, the status bar renders the context
/// indicator. Catches the regression where CostStats accumulated into
/// the wrong field.
///
/// WO 34.3: the status bar now shows `NN% context` only when pressure
/// is >= 50%; below 50% it shows the token count. 42k/128k = 32%, so
/// the bar shows `42.0K tokens`.
#[test]
fn budget_indicator_update() {
    use crate::shared::{ModelInfo, ToolCallStyle};
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    h.state.provider.model_info = Some(ModelInfo {
        name: "qwen2.5".into(),
        supports_thinking: false,
        tool_call_format: ToolCallStyle::Native,
        max_context_tokens: 128_000,
        recommended_temperature: 0.7,
        supports_images: false,
        supports_cache: false,
    });
    h.feed_event(TurnEvent::CostStats {
        prompt_tokens: 42_000,
        completion_tokens: 1_000,
        turn_cost: 0.001,
        cumulative_cost: 0.001,
    });

    let rendered = h.render();
    // WO 34.3: 42k/128k = 32% (comfortable, <50%) → shows token count.
    assert!(
        rendered.contains("42.0K tokens"),
        "token count missing from status bar (got no '42.0K tokens' in render): {rendered}"
    );
    assert_eq!(h.state.budget.last_turn_prompt_tokens, 42_000);
}

/// Scroll handling under volume: 100 messages must render without panic
/// and the most recent message must be visible (auto-scroll pinned to
/// bottom). Catches the max_scroll underflow that panicked on tall
/// content when `visible_height >= total_lines`.
#[test]
fn scroll_100_messages() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    for i in 0..100u32 {
        h.state
            .conversation
            .messages
            .push_back(ConversationEntry::new("user", format!("message {i}")));
    }
    // auto_scroll defaults to true; the renderer pins scroll to the bottom.

    let rendered = h.render();
    assert!(
        rendered.contains("message 99"),
        "latest message should be visible under auto-scroll"
    );
    // Sanity: the renderer published a clamped max_scroll.
    assert!(h.state.conversation.max_scroll < usize::MAX);
}

/// Slash command menu: setting `ui.slash_menu` renders the popup with
/// at least one real command. Catches the empty-popup regression where
/// `complete_command` returned aliases instead of primary triggers.
#[test]
fn slash_command_menu() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    h.state.ui.slash_menu = Some(SlashMenu {
        query: String::new(),
        selected: 0,
    });

    let rendered = h.render();
    // /help is the canonical primary trigger in the command table.
    assert!(
        rendered.contains("/help"),
        "slash menu should list /help, got:\n{rendered}"
    );
    assert!(
        rendered.contains("Commands"),
        "slash menu border title missing"
    );
}

/// Search overlay: entering search mode (`search.mode = true`) routes
/// the input bar through `render_search_bar`. Catches the gate
/// regression where Ctrl+F set the flag but the renderer keyed off
/// a different field.
#[test]
fn search_overlay() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    // Seed a message so render_app takes the chat path (not the welcome
    // screen, which independently shows a "/help" line).
    h.feed_event(TurnEvent::Token("body".into()));
    h.state.search.mode = true;
    h.state.search.query = "needle".into();

    h.assert_contains("Ctrl+F");
    h.assert_contains("needle");
    // Search mode replaces the normal input bar, so the input placeholder
    // must NOT render. Exercises `assert_not_contains` (spec R1) and pins
    // the search-mode gate in `render_input`.
    h.assert_not_contains("Type a message or /help");
}

/// Doom-loop banner: `DoomLoopState` with `count >= THRESHOLD` (3) and
/// `!acknowledged` triggers `render_if_active`. Catches the threshold
/// regression where the banner compared against the wrong constant.
#[test]
fn doom_loop_banner() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    h.state.doom.doom_loop = Some(DoomLoopState {
        count: 3,
        tool: "bash".into(),
        last_error: "permission denied".into(),
        acknowledged: false,
    });

    let rendered = h.render();
    assert!(
        rendered.contains("Doom loop detected"),
        "doom-loop banner title missing"
    );
    assert!(
        rendered.contains("permission denied"),
        "doom-loop banner should show the last error"
    );
    assert!(
        rendered.contains("Break"),
        "doom-loop banner should list the Break action"
    );
}

/// Empty state: a fresh AppState with no messages and no input must
/// render the welcome screen (not a blank chat panel). Catches the
/// regression where the welcome gate checked the wrong field.
#[test]
fn empty_state() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");

    let rendered = h.render();
    // render_welcome emits the spaced banner "k i r k f o r g e".
    assert!(
        rendered.contains("k i r k f o r g e"),
        "welcome banner missing on empty state"
    );
    assert!(
        rendered.contains("/help"),
        "welcome screen should mention /help"
    );
}

/// Belt-and-suspenders: the harness itself renders without panic on a
/// bare default state (no events, no connection, no model). Guards the
/// "fresh AppState must render" contract that every other scenario
/// relies on.
#[test]
fn harness_renders_bare_default_state() {
    let mut h = TuiTestHarness::new();
    let rendered = h.render();
    // Disconnected banner is the default posture; welcome also shows
    // because messages and input are both empty.
    assert!(
        rendered.contains("k i r k f o r g e") || rendered.contains("Disconnected"),
        "bare default state should render welcome or disconnected banner"
    );
}

/// Slash menu must surface command ALIASES, not just primaries. Typing
/// `/q` filters the menu; before the fix `complete_command` only looked
/// at the first trigger of each command, so `/quit` (an alias of
/// `/exit`) never appeared — the user could not discover or complete it.
/// This scenario opens the menu with query `"q"` and asserts `/quit`
/// is listed.
#[test]
fn slash_menu_shows_alias_quit() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    h.state.ui.slash_menu = Some(SlashMenu {
        query: "q".into(),
        selected: 0,
    });

    h.assert_contains("/quit");
    // `/exit` (the primary) does NOT match `q`, so it must be absent
    // from this filtered view — proving the match is on the alias text.
    h.assert_not_contains("/exit");
}

/// Every overlay panel must render through the full pipeline without
/// panic. Catches an overlay widget that derefs empty state (e.g. the
/// Threads daemon picker) and crashes the whole render. WO 34.1 removed
/// the tab bar, so we assert the overlay's own header text is present
/// instead of a tab-bar label.
#[test]
fn tab_panels_render_without_panic() {
    for tab in ActiveTab::OVERLAYS {
        let mut h = TuiTestHarness::new().connected("qwen2.5");
        h.state.ui.active_tab = tab;
        // Must not panic — that's the contract under test.
        let rendered = h.render();
        // Chat overlay (F1) shows the welcome screen or chat; the other
        // overlays show their own header. Chat's label is "Chat" and the
        // header line always contains "kf-code", so check for either the
        // overlay label or the app name.
        if tab == ActiveTab::Chat {
            assert!(
                rendered.contains("kf-code"),
                "chat overlay should show the app header"
            );
        } else {
            assert!(
                rendered.contains(tab.label()),
                "overlay should show its label {}",
                tab.label()
            );
        }
    }
}

/// Switching away from chat-only and back preserves the chat scroll offset
/// (it lives on AppState, not a per-overlay local). Seeds a tall
/// conversation, scrolls up off the bottom, flips to Jobs and back,
/// and checks the offset survived.
#[test]
fn tab_switch_preserves_chat_scroll() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    for i in 0..40u32 {
        h.state
            .conversation
            .messages
            .push_back(ConversationEntry::new("user", format!("line {i}")));
    }
    // Force a render so max_scroll is published, then scroll up.
    h.render();
    assert!(h.state.conversation.max_scroll > 0, "expected scroll range");
    h.state.conversation.auto_scroll = false;
    h.state.conversation.scroll_offset = 0;

    // Flip to Jobs and back to chat-only.
    h.state.ui.active_tab = ActiveTab::Jobs;
    h.render();
    h.state.ui.active_tab = ActiveTab::None;
    h.render();

    assert_eq!(
        h.state.conversation.scroll_offset, 0,
        "scroll offset must survive an overlay round-trip"
    );
}

/// `/help` output is generated from the COMMANDS table and rendered as
/// a system message. Verifies the render path for a long system message
/// and that both `/exit` and `/quit` are listed (catches a help-text
/// regression that dropped aliases).
#[test]
fn help_message_renders_command_list() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    let text = crate::tui::keys::slash_commands::help_text(&h.state.services.skill_registry);
    h.state
        .conversation
        .messages
        .push_back(ConversationEntry::new("system", text));

    // The help text is long; auto_scroll would pin to the bottom
    // (keybindings section) and hide the command list at the top. Scroll
    // to the top so the command listing is in view before asserting.
    h.render(); // publish max_scroll
    h.state.conversation.auto_scroll = false;
    h.state.conversation.scroll_offset = 0;

    h.assert_contains("/exit");
    h.assert_contains("/quit");
    h.assert_contains("Session");
}

/// Streaming markdown renders incrementally without panic. A heading
/// arrives token-by-token (`#`, then ` Title`); every intermediate
/// partial must render, and the finalised heading text must be visible.
/// Guards against the partial-markdown parser path choking on a lone
/// `#` or an unclosed construct mid-stream.
#[test]
fn streaming_heading_renders_incrementally() {
    let mut h = TuiTestHarness::new().connected("qwen2.5");
    // Partial heading — just the opener. Must not panic.
    h.feed_event(TurnEvent::Token("#".into()));
    let _partial = h.render();

    // Complete the heading. The text must now be visible.
    h.feed_event(TurnEvent::Token(" Title".into()));
    h.assert_contains("Title");

    // Streaming a fenced code block opener then content must not panic
    // and must show the code text (the unclosed fence is treated as a
    // code block-in-progress, which is the desired live view).
    h.state.conversation.messages.clear();
    h.state.generation.is_generating = true;
    h.feed_event(TurnEvent::Token("```python\n".into()));
    h.feed_event(TurnEvent::Token("print('hi')".into()));
    h.assert_contains("print");
}

// ─────────────────────────────────────────────────────────────────────
// Key-dispatch scenarios.
//
// The render scenarios above cover the paint path; these cover the
// INPUT path — they drive the same `handle_input_key` the event loop
// calls, so the state transitions are byte-for-byte identical to
// production. Each targets one key-handling bug class.
// ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod key_scenarios {
    use super::*;
    use crate::session::conversation::ConversationLog;
    use crate::session::prompt::CompactRequest;
    use crate::shared::Config;
    use crate::tui::commands::PersonaResult;
    use crate::tui::keys::{handle_input_key, HandleInputContext};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use kf_plugin_host::PluginRegistry;
    use tokio::sync::mpsc;

    /// Owns an `AppState` plus the full set of dispatch channels, so a
    /// test can press keys through the production handler and then
    /// inspect both `state` and what was signaled on the channels
    /// (`cancel_rx`, `input_rx`). Modeled on the existing `keys::tests`.
    pub(super) struct KeyHarness {
        pub state: AppState,
        input_tx: mpsc::UnboundedSender<String>,
        input_rx: mpsc::UnboundedReceiver<String>,
        cancel_tx: mpsc::UnboundedSender<()>,
        cancel_rx: mpsc::UnboundedReceiver<()>,
        resume_tx: mpsc::UnboundedSender<ConversationLog>,
        compact_tx: mpsc::UnboundedSender<CompactRequest>,
        model_tx: mpsc::UnboundedSender<String>,
        undo_tx: mpsc::UnboundedSender<()>,
        config_tx: mpsc::UnboundedSender<Config>,
        plan_tx: mpsc::UnboundedSender<bool>,
        persona_tx: mpsc::UnboundedSender<PersonaResult>,
        event_tx: mpsc::Sender<TurnEvent>,
        plugin_reload_tx: mpsc::UnboundedSender<PluginRegistry>,
    }

    impl KeyHarness {
        fn new() -> Self {
            let (input_tx, input_rx) = mpsc::unbounded_channel();
            let (cancel_tx, cancel_rx) = mpsc::unbounded_channel();
            let (resume_tx, _resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
            let (compact_tx, _compact_rx) = mpsc::unbounded_channel();
            let (model_tx, _model_rx) = mpsc::unbounded_channel();
            let (undo_tx, _undo_rx) = mpsc::unbounded_channel();
            let (config_tx, _config_rx) = mpsc::unbounded_channel::<Config>();
            let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();
            let (persona_tx, _persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
            let (event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
            let (plugin_reload_tx, _plugin_reload_rx) = mpsc::unbounded_channel::<PluginRegistry>();
            Self {
                state: app_state(),
                input_tx,
                input_rx,
                cancel_tx,
                cancel_rx,
                resume_tx,
                compact_tx,
                model_tx,
                undo_tx,
                config_tx,
                plan_tx,
                persona_tx,
                event_tx,
                plugin_reload_tx,
            }
        }

        async fn press(&mut self, code: KeyCode) {
            self.press_with(code, KeyModifiers::NONE).await;
        }

        async fn press_char(&mut self, c: char, mods: KeyModifiers) {
            self.press_with(KeyCode::Char(c), mods).await;
        }

        async fn press_with(&mut self, code: KeyCode, mods: KeyModifiers) {
            let ctx = HandleInputContext {
                input_tx: &self.input_tx,
                cancel_tx: &self.cancel_tx,
                resume_tx: &self.resume_tx,
                compact_tx: &self.compact_tx,
                model_tx: &self.model_tx,
                undo_tx: &self.undo_tx,
                config_tx: &self.config_tx,
                plan_tx: &self.plan_tx,
                persona_tx: &self.persona_tx,
                event_tx: &self.event_tx,
                plugin_reload_tx: &self.plugin_reload_tx,
            };
            handle_input_key(KeyEvent::new(code, mods), &mut self.state, &ctx)
                .await
                .expect("handle_input_key must not error");
        }
    }

    /// `/exit` and `/quit` must both route through the slash dispatcher
    /// and set `session.should_exit`. Catches a regression where the
    /// Enter handler failed to forward an alias to the dispatcher.
    #[tokio::test]
    async fn exit_and_quit_both_set_should_exit() {
        let mut h = KeyHarness::new();
        h.state.conversation.input = "/exit".into();
        h.press(KeyCode::Enter).await;
        assert!(
            h.state.session.should_exit,
            "/exit must set should_exit (end-to-end routing)"
        );

        let mut h = KeyHarness::new();
        h.state.conversation.input = "/quit".into();
        h.press(KeyCode::Enter).await;
        assert!(
            h.state.session.should_exit,
            "/quit (alias) must set should_exit (end-to-end routing)"
        );
    }

    /// Enter on empty input with no messages must be a no-op: no spurious
    /// message, no prompt sent to the executor, no quit. Guards the
    /// invariant that empty submits never reach the model.
    #[tokio::test]
    async fn enter_on_empty_input_with_no_messages_is_noop() {
        let mut h = KeyHarness::new();
        h.press(KeyCode::Enter).await;
        assert!(h.state.conversation.messages.is_empty(), "no message added");
        assert!(!h.state.session.should_exit, "must not quit");
        assert!(!h.state.generation.is_generating, "must not start a turn");
        // Nothing was forwarded to the executor.
        assert!(h.input_rx.try_recv().is_err(), "no prompt sent on input_tx");
    }

    /// Ctrl+C when idle quits the app.
    #[tokio::test]
    async fn ctrl_c_when_idle_quits() {
        let mut h = KeyHarness::new();
        h.press_char('c', KeyModifiers::CONTROL).await;
        assert!(h.state.session.should_exit, "idle Ctrl+C must quit");
    }

    /// First Ctrl+C while generating cancels the turn (signals cancel,
    /// clears the generating flag) WITHOUT quitting; the second then
    /// quits. This is the documented two-press contract.
    #[tokio::test]
    async fn ctrl_c_cancel_then_quit_two_presses() {
        let mut h = KeyHarness::new();
        h.state.generation.is_generating = true;

        // First press: cancel, not quit.
        h.press_char('c', KeyModifiers::CONTROL).await;
        assert!(
            !h.state.generation.is_generating,
            "first Ctrl+C must clear the generating flag"
        );
        assert!(
            !h.state.session.should_exit,
            "first Ctrl+C must NOT quit while generating"
        );
        assert!(
            h.cancel_rx.try_recv().is_ok(),
            "first Ctrl+C must signal cancel on the channel"
        );
        // The cancel must be ANNOUNCED so the two-press contract is
        // explicit — otherwise a "did it work?" second press quits by
        // accident. Guards the clarity fix.
        assert!(
            h.state
                .conversation
                .messages
                .iter()
                .any(|m| m.content.contains("cancelled")),
            "first Ctrl+C should announce the cancel"
        );

        // Second press: nothing is generating now, so it quits.
        h.press_char('c', KeyModifiers::CONTROL).await;
        assert!(h.state.session.should_exit, "second Ctrl+C must quit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `render_to_string` helper produces one line per terminal row.
    /// Guards the row-count contract that the `assert_contains` helpers
    /// implicitly depend on.
    #[test]
    fn render_to_string_has_one_row_per_terminal_line() {
        let mut state = app_state();
        let text = render_to_string(&mut state, 40, 10);
        assert_eq!(text.lines().count(), 10);
    }
}
