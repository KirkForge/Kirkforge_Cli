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
use kf_plugin_host::PluginRegistry;
use tokio::sync::mpsc;

/// One row in the slash-command table.
pub(crate) struct SlashCommand {
    /// All trigger strings that invoke this command (e.g. `["/help", "/h", "/?"]`).
    pub triggers: &'static [&'static str],
    /// One-line description shown in `/help`.
    pub description: &'static str,
    /// Extended usage shown in `/help` (multi-line, optional).
    pub usage: &'static str,
    /// Help-section header this command belongs under (see `GROUPS`).
    pub group: &'static str,
}

// Display order for the grouped `/help` output. A const array (not a
// HashMap) so the section order is deterministic. Every `SlashCommand`
// row must tag itself with one of these strings — the
// `help_text_groups_cover_all_commands` test enforces it.
//
// WO 34.9: collapsed to 3 tiers (Everyday / Advanced / Developer) so
// `/help` surfaces the commands a new user actually needs first. The
// completion popup (`complete_command`) also ranks by tier.
pub(crate) const GROUPS: &[&str] = &["Everyday", "Advanced", "Developer"];

/// Numeric rank for a group name. Lower sorts first in the completion
/// popup and in `/help`. Everyday=0, Advanced=1, Developer=2. Used by
/// `complete_command` so the popup surfaces everyday commands first.
pub(crate) fn group_rank(group: &str) -> u8 {
    match group {
        "Everyday" => 0,
        "Advanced" => 1,
        "Developer" => 2,
        _ => 3,
    }
}

pub(crate) const COMMANDS: &[SlashCommand] = &[
    // ── Everyday (8 commands) ───────────────────────────────────────
    SlashCommand {
        triggers: &["/clear"],
        description: "Clear conversation",
        usage: "",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/exit", "/quit"],
        description: "Quit",
        usage: "",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/help", "/h", "/?"],
        description: "Show available commands",
        usage: "",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/model"],
        description: "Hot-swap the active model (bypasses smart routing)",
        usage: "/model <name>",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/compact"],
        description: "Compact conversation history (destructive — see TUI for stats)",
        usage: "",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/sessions"],
        description: "List/search saved sessions, prune old ones, or delete one by id",
        usage: "/sessions list | search <q> | tree | prune [N] [keep K] | delete <id>",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/commit"],
        description: "Commit changes safely",
        usage: "/commit shows status + suggested message; /commit \"message\" stages all and commits after sanitation checks; /commit --push \"message\" also pushes.",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/undo"],
        description: "Undo the most recent edit_file or write_file",
        usage: "/undo list shows the stack; /undo count prints the depth.",
        group: "Everyday",
    },
    SlashCommand {
        triggers: &["/status"],
        description: "Show model, cost, tokens, and context pressure (one-shot)",
        usage: "",
        group: "Everyday",
    },
    // ── Advanced (15 commands) ──────────────────────────────────────
    SlashCommand {
        triggers: &["/fork"],
        description: "Fork session",
        usage: "/fork list | <label> [count]",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/resume"],
        description: "Resume a fork",
        usage: "/resume <fork-id>",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/save"],
        description: "Save conversation transcript to markdown",
        usage: "/save [path]. Default: next to session log.",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/route"],
        description: "Switch to the model configured for a tier",
        usage: "/route simple|medium|complex",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/thinking"],
        description: "Toggle display of reasoning/thinking blocks",
        usage: "/thinking shows or hides thinking content; Esc also toggles.",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/theme"],
        description: "Switch TUI color theme",
        usage: "/theme [default | dark | light | monokai]\n\
                /theme with no arg cycles through the four built-ins.",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/carryover"],
        description: "Show or clear cross-session carryover profile",
        usage: "/carryover show | clear",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/reload"],
        description: "Reload config.toml and environment overrides",
        usage: "/reload plugins  Re-scan plugin directory.\n\
                /reload skills   Re-scan project SKILL.md files.",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/plugins"],
        description: "Plugin management",
        usage: "/plugins list | enable <n> | disable <n> | toggle <n> | reload | trust <n> <tier> | setup | sources | add <n> <path> | remove <n>",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/workflow"],
        description: "Run a programmable JSON workflow",
        usage: "/workflow run <name> [--parallel], /workflow status, /workflow cancel",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/mcp"],
        description: "Show connected MCP server status and warnings",
        usage: "/mcp shows configured servers, tool counts, and resource/prompt warnings",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/metrics"],
        description: "Show metrics",
        usage: "/metrics shows tool-call/verifier/turn/approval counts",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/verify"],
        description: "Show recent verifier verdicts",
        usage: "/verify shows recent verifier verdicts",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/memory"],
        description: "Memory commands",
        usage: "/memory add <fact> | list | search <query> | rm <name>",
        group: "Advanced",
    },
    SlashCommand {
        triggers: &["/permissions"],
        description: "List, revoke, or clear [A]lways permission rules",
        usage: "/permissions list | revoke <i> | clear",
        group: "Advanced",
    },
    // ── Developer (8 commands) ──────────────────────────────────────
    SlashCommand {
        triggers: &["/jobs"],
        description: "Background bash jobs",
        usage: "/jobs | <id> | clean\n\
                Scheduled jobs: /jobs schedule <spec> bash <cmd>, /jobs scheduled list, /jobs run-now <id>, /jobs logs <id>",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/explore"],
        description: "Fork-isolated research: read-only tools, returns a summary",
        usage: "",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/plan"],
        description: "Fork-isolated plan mode: no shell, returns a step-by-step plan",
        usage: "",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/coder"],
        description: "Fork-isolated implementation: full toolset, returns a summary of changes",
        usage: "",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/implement"],
        description: "Exit plan mode and allow the model to implement the approved plan",
        usage: "",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/test"],
        description: "Run cargo test --no-fail-fast; surface a parsed pass/fail summary",
        usage: "/test <timeout-secs>",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/gh"],
        description: "GitHub integration commands",
        usage: "/gh issue | pr | search | run | file  (run with no args for full usage)",
        group: "Developer",
    },
    SlashCommand {
        triggers: &["/init"],
        description: "Initialize project configuration",
        usage: "/init [--force] — creates .kf-code/config.toml + CLAUDE.md skeleton",
        group: "Developer",
    },
];

/// Return every command trigger (including aliases) whose text starts
/// with `/<prefix>`, ranked by tier (Everyday first, then Advanced,
/// then Developer) and alphabetical within each tier. The `prefix`
/// argument is the text the user typed AFTER the `/` (e.g. `"he"` for
/// `/he`); the returned triggers include the leading `/` (e.g.
/// `"/help"`). Pure, deterministic, no I/O. Used by the Tab-completion
/// handler and the slash-menu popup.
///
/// Aliases are included so `/quit` (an alias of `/exit`) is reachable
/// by typing `/q` — only filtering the primary trigger hid every
/// non-first alias from completion.
///
/// WO 34.9: ranking is tier-first so the popup surfaces everyday
/// commands above advanced/developer ones. Within a tier, alphabetical
/// by trigger (stable secondary sort).
pub(crate) fn complete_command(prefix: &str) -> Vec<&'static str> {
    // Collect (trigger, group_rank) pairs so we can sort by tier then
    // by trigger. flat_map over commands × triggers preserves the
    // group lookup; the rank is the SAME for every alias of a command.
    let mut hits: Vec<(&'static str, u8)> = COMMANDS
        .iter()
        .flat_map(|c| {
            let rank = group_rank(c.group);
            c.triggers.iter().map(move |t| (*t, rank))
        })
        .filter(|(t, _)| {
            t.strip_prefix('/')
                .is_some_and(|rest| rest.starts_with(prefix))
        })
        .collect();
    // Sort by (tier_rank, trigger). `sort_by_key` is stable, so
    // alphabetical order within a tier is preserved from the flat_map
    // emission order — but we make it explicit with a tuple key so a
    // future re-order of COMMANDS can't silently change popup order.
    hits.sort_by_key(|&(t, rank)| (rank, t));
    hits.into_iter().map(|(t, _)| t).collect()
}

/// Generate the `/help` text from the `COMMANDS` table plus static keybinding
/// and mention documentation. Keeping the command listing in the table means
/// we only need to add a row to `COMMANDS` — the help text stays in sync.
///
/// WO 34.9: the 3 tiers are shown with **Everyday** expanded (one row per
/// command with description/usage) and **Advanced** + **Developer** as
/// collapsed one-line summaries (triggers listed inline so every trigger
/// still appears in the text — `help_text_includes_every_command_trigger`
/// stays green). This surfaces the commands a new user needs first without
/// burying the advanced/developer ones.
pub(crate) fn help_text(skill_registry: &SkillRegistry) -> String {
    let mut out = String::from("Built-in commands:\n");
    for group in GROUPS {
        let mut rows: Vec<&SlashCommand> = COMMANDS.iter().filter(|c| c.group == *group).collect();
        rows.sort_by_key(|c| c.triggers[0]);
        out.push_str(&format!("\n{group}:\n"));
        if *group == "Everyday" {
            // Expanded: one row per command with description/usage.
            for cmd in rows {
                let triggers = cmd.triggers.join(" | ");
                let body = if cmd.usage.is_empty() {
                    cmd.description
                } else {
                    cmd.usage
                };
                out.push_str(&format!("  {triggers:10} {body}\n"));
            }
        } else {
            // Collapsed: one line with all triggers, comma-separated.
            // Every trigger still appears in the text so the
            // help_text_includes_every_command_trigger test stays green.
            let triggers: Vec<&str> = rows
                .iter()
                .flat_map(|c| c.triggers.iter().copied())
                .collect();
            out.push_str(&format!("  {}\n", triggers.join(", ")));
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
          \n  The bottom bar shows the model, context pressure (colour-coded: green < 50% = comfortable, yellow 50–80% = consider /compact, red > 80% = compact now), cumulative cost, and the current state (Ready / Generating…). The full breakdown (tokens sent/received, elapsed, skill count, plugin tiers) is available on demand via /status.\n",
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
            state.conversation.messages.clear();
            state.generation.thinking_buffer.clear();
            state.search.matches.clear();
            state.search.match_idx = 0;
            state.conversation.code_block_copy_index = 0;
            Ok(true)
        }
        "/exit" | "/quit" => {
            send_or_warn!(ctx.cancel_tx.send(()), "cancel channel receiver dropped");
            state.session.should_exit = true;
            Ok(true)
        }
        "/help" | "/h" | "/?" => {
            // WO 34.2: open the help overlay instead of pushing help text
            // into the conversation. The overlay renders `help_text()`
            // output on top of the chat; Esc closes, ↑/↓ scrolls. This
            // keeps the conversation + session log free of help docs.
            state.ui.help_overlay_visible = true;
            state.ui.help_overlay_scroll = 0;
            state.mark_dirty();
            Ok(true)
        }
        "/fork" => {
            let msg = crate::tui::commands::handle_fork_command(args, state).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/resume" => {
            let msg = crate::tui::commands::handle_resume_command(args, state, ctx.resume_tx).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/jobs" => {
            let msg = crate::tui::commands::handle_jobs_command(args, state).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/status" => {
            let msg = crate::tui::commands::handle_status_command(args, state).await;
            state
                .conversation
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
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/model" => {
            let msg =
                crate::tui::commands::handle_model_command(args, ctx.model_tx, ctx.event_tx, state)
                    .await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/compact" => {
            let msg = crate::tui::commands::handle_compact_command(args, ctx.compact_tx).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/route" => {
            let msg =
                crate::tui::commands::handle_route_command(args, ctx.model_tx, ctx.event_tx, state)
                    .await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/memory" => {
            let msg = crate::tui::commands::handle_memory_command(args);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/metrics" => {
            let msg = crate::tui::commands::handle_metrics_command();
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/verify" => {
            // WO 11.7: show recent verifier verdicts from the metrics log.
            let msg = crate::shared::metrics::format_verifier_report(20);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/save" => {
            let msg = crate::tui::commands::handle_save_command(args, state).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/undo" => {
            let msg = crate::tui::commands::handle_undo_command(args, ctx.undo_tx, state);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/permissions" => {
            let msg = handle_permissions_command(args, state);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/thinking" => {
            state.generation.thinking_panel_visible = !state.generation.thinking_panel_visible;
            let status = if state.generation.thinking_panel_visible {
                "shown"
            } else {
                "hidden"
            };
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
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
                .conversation
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
                .conversation
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
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/implement" => {
            send_or_warn!(
                ctx.plan_tx.send(false),
                "plan-mode channel receiver dropped"
            );
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
                    "system",
                    "✅ Plan mode disabled — implementation may begin.".to_string(),
                ));
            Ok(true)
        }
        "/gh" => {
            let msg = crate::tui::commands::handle_gh_command(args);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/init" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let msg = crate::tui::commands::handle_init_command(args, &cwd);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/commit" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let cfg = crate::shared::read_shared_config(&state.services.config).clone();
            let msg = crate::tui::commands::handle_commit_command(args, &cwd, &cfg).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/sessions" => {
            let msg = crate::tui::commands::handle_sessions_command(args, state);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/carryover" => {
            let msg = crate::tui::commands::handle_carryover_command(args, state);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/test" => {
            let msg = crate::tui::commands::handle_test_command(args, state).await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/plugins" => {
            let msg =
                crate::tui::commands::handle_plugins_command(args, state, ctx.plugin_reload_tx)
                    .await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/workflow" => {
            let msg =
                crate::tui::commands::handle_workflow_command(args, state, ctx.persona_tx.clone())
                    .await;
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        "/mcp" => {
            let cfg = crate::shared::read_shared_config(&state.services.config);
            let servers = &cfg.tools.mcp_servers;
            if servers.is_empty() {
                state
                    .conversation
                    .messages
                    .push_back(ConversationEntry::new(
                        "system",
                        "No MCP servers configured.".to_string(),
                    ));
            } else {
                let mut lines = vec![format!("{} MCP server(s) configured:", servers.len())];
                for s in servers {
                    lines.push(format!("  {} ({})", s.name, s.transport));
                }
                state
                    .conversation
                    .messages
                    .push_back(ConversationEntry::new("system", lines.join("\n")));
            }
            Ok(true)
        }
        "/theme" => {
            let msg = handle_theme_command(args, state);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", msg));
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Handle `/permissions list | revoke <i> | clear`.
///
/// `list` (default when no arg is given) prints the rules with
/// 1-indexed positions and does not mutate. `revoke <i>` and `clear`
/// mutate `Config.security.permission_rules` in the shared config and
/// persist via `save_config` so the change survives across sessions.
/// The pure ops layer lives in `src/tui/commands/permissions.rs`.
fn handle_permissions_command(args: &str, state: &mut AppState) -> String {
    use crate::tui::commands::permissions as ops;
    let trimmed = args.trim();
    let mut tokens = trimmed.split_whitespace();
    let sub = tokens.next().unwrap_or("list");

    match sub {
        "list" | "" => ops::list(&crate::shared::read_shared_config(&state.services.config)),
        "clear" => {
            let msg = {
                let mut cfg = state
                    .services
                    .config
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                ops::clear(&mut cfg)
            };
            match persist_shared(&state.services.config) {
                Ok(()) => msg,
                Err(e) => format!("{msg}\n⚠️ Failed to persist config: {e}"),
            }
        }
        "revoke" => {
            let idx_str = match tokens.next() {
                Some(s) => s,
                None => return "Usage: /permissions revoke <i>".to_string(),
            };
            if tokens.next().is_some() {
                return "Usage: /permissions revoke <i>".to_string();
            }
            let idx: usize = match idx_str.parse() {
                Ok(n) => n,
                Err(_) => {
                    return format!("Usage: /permissions revoke <i>\n`{idx_str}` is not a number");
                }
            };
            let result = {
                let mut cfg = state
                    .services
                    .config
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                ops::revoke(&mut cfg, idx)
            };
            match result {
                Ok(msg) => match persist_shared(&state.services.config) {
                    Ok(()) => msg,
                    Err(e) => format!("{msg}\n⚠️ Failed to persist config: {e}"),
                },
                Err(e) => format!("❌ {e}"),
            }
        }
        other => {
            format!("Usage: /permissions list | revoke <i> | clear\nUnknown subcommand '{other}'")
        }
    }
}

/// Persist the shared config to disk via the existing config-write path.
fn persist_shared(cfg: &crate::shared::SharedConfig) -> anyhow::Result<()> {
    let snapshot = crate::shared::read_shared_config(cfg).clone();
    crate::session::config::save_config(&snapshot)
}

/// Handle `/theme [name]` (WO 27.6).
///
/// No-arg cycles default→dark→light→monokai→default. With-arg sets the
/// theme if the name matches a built-in; otherwise prints the available
/// list. On a successful switch: updates `state.ui.theme`, clears the
/// chat render cache (so all entries rebuild with new colors), and
/// persists the choice to `display.theme` in config.toml.
fn handle_theme_command(args: &str, state: &mut AppState) -> String {
    use crate::tui::theme::Theme;

    let current = crate::shared::read_shared_config(&state.services.config)
        .display
        .theme
        .clone();
    let trimmed = args.trim();
    let next: String = if trimmed.is_empty() {
        Theme::next_name(&current).to_string()
    } else {
        trimmed.to_string()
    };

    if !Theme::BUILTIN_NAMES.contains(&next.as_str()) {
        return format!(
            "Unknown theme '{next}'. Available: {}.",
            Theme::BUILTIN_NAMES.join(", ")
        );
    }

    state.ui.theme = Theme::from_name(&next);
    // Cache invalidation: rendered Lines carry the old colors as Style
    // state, so they must be rebuilt. clear_entries is O(entries).
    state.conversation.chat_render_cache.clear_entries();
    state.mark_dirty();

    {
        let mut cfg = state
            .services
            .config
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cfg.display.theme = next.clone();
    }
    match persist_shared(&state.services.config) {
        Ok(()) => format!("Theme: {next} (saved to config.toml)."),
        Err(e) => format!(
            "Theme: {next} for this session.\n\
             ⚠️ Failed to persist to config.toml: {e}"
        ),
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
            "/permissions",
            "/thinking",
            "/reload",
            "/sessions",
            "/carryover",
            "/test",
            "/memory",
            "/metrics",
            "/verify",
            "/gh",
            "/init",
            "/plugins",
            "/workflow",
            "/mcp",
            "/theme",
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

    #[test]
    fn help_text_includes_group_headers() {
        let registry = SkillRegistry::new();
        let text = help_text(&registry);
        for group in GROUPS {
            let header = format!("\n{group}:\n");
            assert!(
                text.contains(&header),
                "help text missing group header {group:?}"
            );
        }
    }

    #[test]
    fn help_text_groups_cover_all_commands() {
        // Every command must be tagged with a known group, and every
        // group must list at least one command. Catches a future row
        // that forgets the `group` field or invents a new group name
        // without adding it to `GROUPS`.
        for cmd in COMMANDS {
            assert!(
                GROUPS.contains(&cmd.group),
                "command {:?} has unknown group {:?} — add it to GROUPS",
                cmd.triggers[0],
                cmd.group,
            );
        }
        for group in GROUPS {
            assert!(
                COMMANDS.iter().any(|c| c.group == *group),
                "group {group:?} has no commands — remove it from GROUPS or tag a command",
            );
        }
    }

    #[test]
    fn help_text_no_empty_usage_for_documented_commands() {
        // The six commands that shipped with `usage: ""` in WO 14.1's
        // base now carry concrete syntax. A regression here means
        // someone blanked a usage string — the user would see the
        // command exists but not how to call it.
        let documented = ["/memory", "/metrics", "/verify", "/gh", "/init", "/plugins"];
        for trigger in documented {
            let cmd = COMMANDS
                .iter()
                .find(|c| c.triggers.contains(&trigger))
                .unwrap_or_else(|| panic!("command {trigger} missing from COMMANDS"));
            assert!(
                !cmd.usage.is_empty(),
                "{trigger} usage is empty — fill it with the real syntax"
            );
        }
    }

    #[test]
    fn complete_command_he_returns_help() {
        // "he" uniquely matches "/help" via the primary trigger.
        // Aliases ("/h", "/?") are NOT returned — `complete_command`
        // returns the primary (first alias) for each command.
        assert_eq!(complete_command("he"), vec!["/help"]);
    }

    #[test]
    fn complete_command_p_returns_multiple() {
        // "p" matches the primary triggers of several commands
        // (/plan, /plugins, ...). The exact set depends on what is
        // in COMMANDS at merge time — pin that it is at least two.
        let matches = complete_command("p");
        assert!(
            matches.len() >= 2,
            "expected >=2 matches for \"p\", got {matches:?}"
        );
        assert!(matches.iter().all(|t| t.starts_with("/p")));
        // No duplicates — each command contributes at most its primary.
        let mut sorted = matches.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            matches.len(),
            "duplicate primaries in {matches:?}"
        );
        // /plan and /plugins are stable across the WO 14.x series.
        assert!(matches.contains(&"/plan"), "missing /plan in {matches:?}");
        assert!(
            matches.contains(&"/plugins"),
            "missing /plugins in {matches:?}"
        );
    }

    #[test]
    fn complete_command_quiet_matches_quit_alias() {
        // `/quit` is an alias of `/exit`. Completion must surface aliases,
        // not just primaries — otherwise `/q` shows nothing and the user
        // cannot discover `/quit`.
        assert_eq!(complete_command("q"), vec!["/quit"]);
        assert_eq!(complete_command("quit"), vec!["/quit"]);
    }

    #[test]
    fn complete_command_zzz_returns_empty() {
        // No command starts with "zzz".
        assert!(complete_command("zzz").is_empty());
    }

    #[test]
    fn complete_command_empty_prefix_returns_all_triggers() {
        // An empty prefix matches every trigger — INCLUDING aliases —
        // so the count is the total alias count, not the command count.
        let all = complete_command("");
        let total_aliases: usize = COMMANDS.iter().map(|c| c.triggers.len()).sum();
        assert!(!all.is_empty());
        assert_eq!(
            all.len(),
            total_aliases,
            "empty prefix should return every alias"
        );
        // Every command's primary (first alias) is present.
        for cmd in COMMANDS {
            assert!(
                all.contains(&cmd.triggers[0]),
                "primary {:?} missing from empty-prefix completion",
                cmd.triggers[0]
            );
        }
        // No duplicates.
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "duplicate triggers in {all:?}");
    }

    // ── WO 34.9: tier-ranked completion ordering ────────────────────

    /// Helper: look up the group of a command by its primary trigger.
    fn group_of(trigger: &str) -> &'static str {
        COMMANDS
            .iter()
            .find(|c| c.triggers.contains(&trigger))
            .map(|c| c.group)
            .expect("trigger not found in COMMANDS")
    }

    /// Empty-prefix completion must return Everyday commands BEFORE
    /// Advanced, and Advanced BEFORE Developer. Within a tier the
    /// order is alphabetical by trigger. This is the WO 34.9 ranking
    /// contract — the popup surfaces everyday commands first.
    #[test]
    fn complete_command_ranks_by_tier_everyday_first() {
        let all = complete_command("");
        // Find the index of the first Advanced and first Developer trigger.
        let first_advanced = all
            .iter()
            .position(|t| group_of(t) == "Advanced")
            .expect("no Advanced trigger in completion");
        let first_developer = all
            .iter()
            .position(|t| group_of(t) == "Developer")
            .expect("no Developer trigger in completion");
        // Every Everyday trigger must come before the first Advanced one.
        for t in &all[..first_advanced] {
            assert_eq!(
                group_of(t),
                "Everyday",
                "Everyday tier leaked an Advanced/Developer trigger: {t}"
            );
        }
        // Every Advanced trigger must come before the first Developer one.
        for t in &all[first_advanced..first_developer] {
            assert_eq!(
                group_of(t),
                "Advanced",
                "Advanced tier leaked a Developer/Everyday trigger: {t}"
            );
        }
        // Developer tier is last.
        for t in &all[first_developer..] {
            assert_eq!(
                group_of(t),
                "Developer",
                "Developer tier leaked an Everyday/Advanced trigger: {t}"
            );
        }
    }

    /// `/help` must show Everyday expanded (one row per command) and
    /// Advanced + Developer collapsed (one line each, triggers listed
    /// inline). Catches the regression where a tier gets blanked or
    /// the expansion logic flips.
    #[test]
    fn help_text_everyday_expanded_advanced_developer_collapsed() {
        let registry = SkillRegistry::new();
        let text = help_text(&registry);
        // Everyday: /clear's description must appear (expanded form).
        assert!(
            text.contains("Clear conversation"),
            "Everyday tier should be expanded with descriptions"
        );
        // Advanced: triggers appear inline on one line (collapsed form).
        // The /workflow trigger is Advanced — it should appear in the
        // collapsed listing.
        assert!(
            text.contains("/workflow"),
            "Advanced tier should list /workflow inline"
        );
        // Developer: /plan trigger is Developer — collapsed inline.
        assert!(
            text.contains("/plan"),
            "Developer tier should list /plan inline"
        );
        // The Everyday expansion puts /clear on its own row with the
        // description; the collapsed Advanced/Developer lines do NOT
        // include descriptions. So "Run cargo test" (a Developer
        // description) must NOT appear in the help text.
        assert!(
            !text.contains("Run cargo test --no-fail-fast"),
            "Developer tier should be collapsed (no descriptions)"
        );
    }

    /// `group_rank` is the ordering primitive for `complete_command`.
    /// Pin the 3-tier mapping so a future rename can't silently break
    /// the ranking.
    #[test]
    fn group_rank_orders_everyday_before_advanced_before_developer() {
        assert!(group_rank("Everyday") < group_rank("Advanced"));
        assert!(group_rank("Advanced") < group_rank("Developer"));
        assert_eq!(group_rank("unknown"), 3, "unknown groups sort last");
    }
}
