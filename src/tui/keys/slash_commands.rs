//! Table-driven slash-command dispatch.
//!
//! The `COMMANDS` table lists every built-in slash command with its trigger
//! aliases and a one-line description. The `/help` text is generated from
//! this table so that adding a new command only requires an entry here and
//! a match arm in `dispatch_slash_command` — the help text stays in sync
//! automatically.

use crate::send_or_warn;
use crate::session::conversation::ConversationLog;
use crate::session::prompt::CompactRequest;
use crate::session::skills::SkillRegistry;
use crate::shared::Config;
use crate::tui::app::{AppState, ConversationEntry};
use crate::tui::commands::{PersonaKind, PersonaResult};
use kirkforge_plugin_host::PluginRegistry;
use tokio::sync::mpsc;

/// One row in the slash-command table.
pub(crate) struct SlashCommand {
    /// All trigger strings that invoke this command (e.g. `["/help", "/h", "/?"]`).
    pub triggers: &'static [&'static str],
    /// One-line description shown in `/help`.
    pub description: &'static str,
    /// Extended usage shown in `/help` (multi-line, optional).
    pub usage: &'static str,
}

pub(crate) const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        triggers: &["/clear"],
        description: "Clear conversation",
        usage: "",
    },
    SlashCommand {
        triggers: &["/exit", "/quit"],
        description: "Quit",
        usage: "",
    },
    SlashCommand {
        triggers: &["/help", "/h", "/?"],
        description: "Show available commands",
        usage: "",
    },
    SlashCommand {
        triggers: &["/fork"],
        description: "Fork session",
        usage: "/fork list | <label> [count]",
    },
    SlashCommand {
        triggers: &["/resume"],
        description: "Resume a fork",
        usage: "/resume <fork-id>",
    },
    SlashCommand {
        triggers: &["/jobs"],
        description: "Background bash jobs",
        usage: "/jobs | <id> | clean\n\
                Scheduled jobs: /jobs schedule <spec> bash <cmd>, /jobs scheduled list, /jobs run-now <id>, /jobs logs <id>",
    },
    SlashCommand {
        triggers: &["/status"],
        description: "Show model, cost, tokens, and context pressure (one-shot)",
        usage: "",
    },
    SlashCommand {
        triggers: &["/model"],
        description: "Hot-swap the active model (bypasses smart routing)",
        usage: "/model <name>",
    },
    SlashCommand {
        triggers: &["/route"],
        description: "Switch to the model configured for a tier",
        usage: "/route simple|medium|complex",
    },
    SlashCommand {
        triggers: &["/compact"],
        description: "Compact conversation history (destructive — see TUI for stats)",
        usage: "",
    },
    SlashCommand {
        triggers: &["/save"],
        description: "Save conversation transcript to markdown",
        usage: "/save [path]. Default: next to session log.",
    },
    SlashCommand {
        triggers: &["/explore"],
        description: "Fork-isolated research: read-only tools, returns a summary",
        usage: "",
    },
    SlashCommand {
        triggers: &["/plan"],
        description: "Fork-isolated plan mode: no shell, returns a step-by-step plan",
        usage: "",
    },
    SlashCommand {
        triggers: &["/coder"],
        description: "Fork-isolated implementation: full toolset, returns a summary of changes",
        usage: "",
    },
    SlashCommand {
        triggers: &["/implement"],
        description: "Exit plan mode and allow the model to implement the approved plan",
        usage: "",
    },
    SlashCommand {
        triggers: &["/commit"],
        description: "Commit changes safely",
        usage: "/commit shows status + suggested message; /commit \"message\" stages all and commits after sanitation checks; /commit --push \"message\" also pushes.",
    },
    SlashCommand {
        triggers: &["/undo"],
        description: "Undo the most recent edit_file or write_file",
        usage: "/undo list shows the stack; /undo count prints the depth.",
    },
    SlashCommand {
        triggers: &["/thinking"],
        description: "Toggle display of reasoning/thinking blocks",
        usage: "/thinking shows or hides thinking content; Esc also toggles.",
    },
    SlashCommand {
        triggers: &["/reload"],
        description: "Reload config.toml and environment overrides",
        usage: "/reload plugins  Re-scan plugin directory.\n\
                /reload skills   Re-scan project SKILL.md files.",
    },
    SlashCommand {
        triggers: &["/sessions"],
        description: "List/search saved sessions, prune old ones, or delete one by id",
        usage: "",
    },
    SlashCommand {
        triggers: &["/carryover"],
        description: "Show or clear cross-session carryover profile",
        usage: "",
    },
    SlashCommand {
        triggers: &["/test"],
        description: "Run cargo test --no-fail-fast; surface a parsed pass/fail summary",
        usage: "/test <timeout-secs>",
    },
    SlashCommand {
        triggers: &["/memory"],
        description: "Memory commands",
        usage: "",
    },
    SlashCommand {
        triggers: &["/metrics"],
        description: "Show metrics",
        usage: "",
    },
    SlashCommand {
        triggers: &["/gh"],
        description: "GitHub integration commands",
        usage: "",
    },
    SlashCommand {
        triggers: &["/init"],
        description: "Initialize project configuration",
        usage: "",
    },
    SlashCommand {
        triggers: &["/plugins"],
        description: "Plugin management",
        usage: "",
    },
    SlashCommand {
        triggers: &["/workflow"],
        description: "Run a programmable JSON workflow",
        usage: "/workflow run <name>, /workflow status, /workflow cancel",
    },
];

/// Generate the `/help` text from the `COMMANDS` table plus static keybinding
/// and mention documentation. Keeping the command listing in the table means
/// we only need to add a row to `COMMANDS` — the help text stays in sync.
pub(crate) fn help_text(skill_registry: &SkillRegistry) -> String {
    let mut out = String::from("Built-in commands:\n");
    for cmd in COMMANDS {
        let triggers = cmd.triggers.join(" | ");
        if cmd.usage.is_empty() {
            out.push_str(&format!("  {:10} {}\n", triggers, cmd.description));
        } else {
            out.push_str(&format!("  {:10} {}\n", triggers, cmd.usage));
        }
    }
    out.push_str(
        "\nBash passthrough:\n\
         \n  !<command>  Run a shell command directly — no model round trip. Approval is configurable via `bang_requires_approval`. Output is shown as a collapsible tool entry. 30-second timeout; for long jobs use `!<cmd> &` and check /jobs.\n\
         \n@-mentions (inline file context):\n\
         \n  @<path>          Inline the file's contents into the prompt (minified by default). The TUI shows a status row per mention.\n\
         \n  @<path>:raw      Inline the file verbatim, no minification.\n\
         \n  @<path>:A-B      Inline lines A–B (1-indexed, inclusive on both ends).\n\
         \n  @<path>:A-B:raw  Range + verbatim, combined.\n\
         \n  @~/...           Tilde expansion supported (e.g. @~/notes.md).\n\
         \n  Multiple @<path> tokens in one input are all expanded. Each mention is capped at 50 KB (head + tail + marker) and respects the same path-safety rules as the model's read_file tool. Failures (missing, denied, I/O) are shown in the TUI as ✗ rows and as quoted placeholders in the prompt, so the model can react.\n\
         \nKeybindings:\n\
         \n  Ctrl+T   Toggle tool output collapse (default ON)\n\
         \n  Ctrl+F   Search the conversation (Enter to commit and jump, n / Shift+N to cycle, Esc to cancel)\n\
         \n  Enter    Expand/collapse the most recent message (when input is empty)\n\
         \n  Tab      Same as Enter (alternative expand gesture)\n\
         \n  Ctrl+C   Cancel generation + clear input\n\
         \n  Ctrl+Shift+C  Copy last assistant message to clipboard\n\
         \n  Ctrl+Shift+B  Copy a code block from the most recent assistant message (repeat to cycle blocks)\n\
         \n  Ctrl+W   Delete word backward\n\
         \n  Ctrl+U   Clear input line\n\
         \n  Esc      Toggle thinking panel (or cancel search if Ctrl+F is active; same as /thinking)\n\
         \nStatus bar:\n\
         \n  The bottom bar shows session model, time, cumulative cost, and a colour-coded budget indicator. Green (< 50%) = comfortable, yellow (50–80%) = consider /compact, red (> 80%) = compact now. The same data is available on demand via /status.\n",
    );
    let skills = skill_registry.all();
    if !skills.is_empty() {
        out.push_str("\nSkills:\n");
        for skill in skills {
            out.push_str(&format!(
                "  {}  — {}{}\n",
                skill.meta.trigger,
                skill.meta.description,
                skill
                    .meta
                    .model
                    .as_ref()
                    .map(|m| format!(" [{m}]"))
                    .unwrap_or_default(),
            ));
        }
    }
    out
}

/// All channel endpoints the slash-command dispatch needs (a subset of
/// [`super::HandleInputContext`]).
pub(crate) struct SlashContext<'a> {
    pub cancel_tx: &'a mpsc::UnboundedSender<()>,
    pub resume_tx: &'a mpsc::UnboundedSender<ConversationLog>,
    pub compact_tx: &'a mpsc::UnboundedSender<CompactRequest>,
    pub model_tx: &'a mpsc::UnboundedSender<String>,
    pub undo_tx: &'a mpsc::UnboundedSender<()>,
    pub config_tx: &'a mpsc::UnboundedSender<Config>,
    pub plan_tx: &'a mpsc::UnboundedSender<bool>,
    pub persona_tx: &'a mpsc::UnboundedSender<PersonaResult>,
    pub event_tx: &'a mpsc::Sender<crate::session::executor::TurnEvent>,
    pub plugin_reload_tx: &'a mpsc::UnboundedSender<PluginRegistry>,
}

/// Dispatch a slash command. Returns `Ok(true)` if the command was handled
/// (including unknown-command messages), `Ok(false)` if the command should
/// fall through to the skill registry.
pub(crate) async fn dispatch_slash_command(
    cmd: &str,
    args: &str,
    state: &mut AppState,
    ctx: &SlashContext<'_>,
) -> anyhow::Result<bool> {
    match cmd {
        "/clear" => {
            state.messages.clear();
            state.thinking_buffer.clear();
            state.search_matches.clear();
            state.search_match_idx = 0;
            state.code_block_copy_index = 0;
            Ok(true)
        }
        "/exit" | "/quit" => {
            send_or_warn!(ctx.cancel_tx.send(()), "cancel channel receiver dropped");
            state.should_exit = true;
            Ok(true)
        }
        "/help" | "/h" | "/?" => {
            state.messages.push_back(ConversationEntry::new(
                "system",
                help_text(&state.skill_registry),
            ));
            Ok(true)
        }
        "/fork" => {
            let msg = crate::tui::commands::handle_fork_command(args, state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/resume" => {
            let msg = crate::tui::commands::handle_resume_command(args, state, ctx.resume_tx).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/jobs" => {
            let msg = crate::tui::commands::handle_jobs_command(args, state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/status" => {
            let msg = crate::tui::commands::handle_status_command(args, state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/reload" => {
            let a = args.trim();
            let msg = match a {
                "plugins" => {
                    crate::tui::commands::handle_reload_plugins_command(ctx.plugin_reload_tx, state)
                        .await
                }
                "skills" => crate::tui::commands::handle_reload_skills_command(state),
                _ => crate::tui::commands::handle_reload_command(ctx.config_tx, state).await,
            };
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/model" => {
            let msg =
                crate::tui::commands::handle_model_command(args, ctx.model_tx, ctx.event_tx, state)
                    .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/compact" => {
            let msg = crate::tui::commands::handle_compact_command(args, ctx.compact_tx).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/route" => {
            let msg =
                crate::tui::commands::handle_route_command(args, ctx.model_tx, ctx.event_tx, state)
                    .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/memory" => {
            let msg = crate::tui::commands::handle_memory_command(args);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/metrics" => {
            let msg = crate::tui::commands::handle_metrics_command();
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/save" => {
            let msg = crate::tui::commands::handle_save_command(args, state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/undo" => {
            let msg = crate::tui::commands::handle_undo_command(args, ctx.undo_tx, state);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/thinking" => {
            state.thinking_panel_visible = !state.thinking_panel_visible;
            let status = if state.thinking_panel_visible {
                "shown"
            } else {
                "hidden"
            };
            state.messages.push_back(ConversationEntry::new(
                "system",
                format!("Thinking blocks are now {status}. Press Esc to toggle."),
            ));
            Ok(true)
        }
        "/plan" => {
            let msg = crate::tui::commands::start_persona(
                PersonaKind::Plan,
                args,
                state,
                ctx.persona_tx.clone(),
            )
            .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/explore" => {
            let msg = crate::tui::commands::start_persona(
                PersonaKind::Explore,
                args,
                state,
                ctx.persona_tx.clone(),
            )
            .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/coder" => {
            let msg = crate::tui::commands::start_persona(
                PersonaKind::Coder,
                args,
                state,
                ctx.persona_tx.clone(),
            )
            .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/implement" => {
            send_or_warn!(
                ctx.plan_tx.send(false),
                "plan-mode channel receiver dropped"
            );
            state.messages.push_back(ConversationEntry::new(
                "system",
                "✅ Plan mode disabled — implementation may begin.".to_string(),
            ));
            Ok(true)
        }
        "/gh" => {
            let msg = crate::tui::commands::handle_gh_command(args);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/init" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let msg = crate::tui::commands::handle_init_command(args, &cwd);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/commit" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let cfg = crate::shared::read_shared_config(&state.config).clone();
            let msg = crate::tui::commands::handle_commit_command(args, &cwd, &cfg).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/sessions" => {
            let msg = crate::tui::commands::handle_sessions_command(args, state);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/carryover" => {
            let msg = crate::tui::commands::handle_carryover_command(args, state);
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/test" => {
            let msg = crate::tui::commands::handle_test_command(args, state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/plugins" => {
            let msg =
                crate::tui::commands::handle_plugins_command(args, state, ctx.plugin_reload_tx)
                    .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/workflow" => {
            let msg =
                crate::tui::commands::handle_workflow_command(args, state, ctx.persona_tx.clone())
                    .await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_command_table_covers_all_triggers() {
        let all_triggers: Vec<&&str> = COMMANDS.iter().flat_map(|c| c.triggers).collect();
        let known = [
            "/clear",
            "/exit",
            "/quit",
            "/help",
            "/h",
            "/?",
            "/fork",
            "/resume",
            "/jobs",
            "/status",
            "/model",
            "/route",
            "/compact",
            "/save",
            "/explore",
            "/plan",
            "/coder",
            "/implement",
            "/commit",
            "/undo",
            "/thinking",
            "/reload",
            "/sessions",
            "/carryover",
            "/test",
            "/memory",
            "/metrics",
            "/gh",
            "/init",
            "/plugins",
            "/workflow",
        ];
        for trigger in known {
            assert!(
                all_triggers.iter().any(|t| **t == trigger),
                "trigger {trigger:?} not found in COMMANDS table"
            );
        }
        for trigger in &all_triggers {
            assert!(
                known.contains(*trigger),
                "COMMANDS table contains trigger {trigger:?} not in known set — add it to the test",
            );
        }
    }

    #[test]
    fn help_text_includes_every_command_trigger() {
        let registry = SkillRegistry::new();
        let text = help_text(&registry);
        for cmd in COMMANDS {
            for trigger in cmd.triggers {
                assert!(
                    text.contains(*trigger),
                    "help text missing trigger {trigger:?}"
                );
            }
        }
    }
}
