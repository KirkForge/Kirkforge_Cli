//! Fork-isolated subagent personas: `/explore`, `/plan`, `/coder`.
//!
//! Each persona creates a session fork, runs a restricted executor in a
//! background task, and returns only the final assistant turn to the main
//! conversation. This gives the model isolated context for research,
//! architecture, or focused coding without polluting the primary thread
//! until the result is merged.
//!
//! # Tool restrictions
//!
//! - `/explore` — read-only discovery tools plus read-only `bash` (enforced
//!   by reusing the executor's `plan_mode` flag inside the fork).
//! - `/plan` — no shell at all; only `read_file`, `read_image`, `grep`, `glob`.
//! - `/coder` — full toolset, isolated context.

use crate::adapters;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{ApprovalRequest, ApprovalResponse, Executor};
use crate::session::toolset::{CompositeToolset, VecToolset};
use crate::shared::{read_shared_config, Config, Role, SharedConfig};
use crate::tools::{Tool, UndoStackRef};
use crate::tui::app::AppState;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Identifies a built-in subagent persona.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaKind {
    /// Read-only research assistant.
    Explore,
    /// Architecture/planning assistant with no shell.
    Plan,
    /// Full-toolset implementation assistant.
    Coder,
}

impl std::fmt::Display for PersonaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaKind::Explore => write!(f, "explore"),
            PersonaKind::Plan => write!(f, "plan"),
            PersonaKind::Coder => write!(f, "coder"),
        }
    }
}

/// Handle to a persona currently running in the background.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PersonaHandle {
    pub kind: PersonaKind,
    pub task: String,
    pub fork_id: String,
}

/// Outcome of a completed persona run, sent back to the TUI event loop.
#[derive(Debug, Clone)]
pub struct PersonaResult {
    pub kind: PersonaKind,
    pub task: String,
    #[allow(dead_code)]
    pub fork_path: PathBuf,
    pub success: bool,
    /// Final assistant content to merge into the parent conversation.
    pub summary: String,
    pub error: Option<String>,
}

/// Render the user-facing prompt for a persona.
fn build_persona_prompt(kind: PersonaKind, task: &str) -> String {
    match kind {
        PersonaKind::Explore => format!(
            "You are an exploratory research assistant. Your job is to read files, \
             search the codebase, and gather context. Use read_file, read_image, \
             grep, glob, and read-only bash only. Do not edit files or run \
             destructive commands. Produce a concise summary of findings.\n\n\
             Task: {task}"
        ),
        PersonaKind::Plan => format!(
            "You are a software architect. Explore the codebase with read-only tools \
             (read_file, read_image, grep, glob). Do not run shell commands. Design \
             a step-by-step implementation plan with specific file paths, risks, \
             and architectural decisions. End with: \"## Plan Complete — ready to implement\".\n\n\
             Task: {task}"
        ),
        PersonaKind::Coder => format!(
            "You are a focused implementation assistant. You have the full toolset \
             including file edits and shell commands. Work efficiently in this \
             isolated context and produce a concise summary of what you changed \
             and why when done.\n\n\
             Task: {task}"
        ),
    }
}

/// Restrict the available tools for a persona kind.
fn tools_for_persona(
    kind: PersonaKind,
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
    config: &Config,
) -> Vec<Arc<dyn Tool>> {
    let (deny_list, path_guard, _read_gate) = crate::session::access::access_from_config(config);
    let all = crate::tools::all_tools(
        undo_stack,
        supports_images,
        deny_list,
        path_guard,
        config.bash_sandbox_workdir,
    );
    match kind {
        PersonaKind::Explore => all
            .into_iter()
            .filter(|t| {
                matches!(
                    t.def().name,
                    "read_file"
                        | "read_image"
                        | "grep"
                        | "glob"
                        | "bash"
                        | "bash_status"
                        | "bash_cancel"
                )
            })
            .collect(),
        PersonaKind::Plan => all
            .into_iter()
            .filter(|t| matches!(t.def().name, "read_file" | "read_image" | "grep" | "glob"))
            .collect(),
        PersonaKind::Coder => all,
    }
}

/// Run a persona in a fork and return the final assistant summary.
///
/// This is executed inside the spawned tokio task; it builds a fresh
/// executor with the restricted toolset, runs one turn (which may itself
/// loop over multiple tool-call iterations), and extracts the last
/// assistant message from the fork log.
#[allow(clippy::too_many_arguments)]
async fn run_persona_task(
    kind: PersonaKind,
    task: String,
    fork_path: PathBuf,
    max_turns: usize,
    model_name: String,
    ollama_host: String,
    config: Config,
    supports_images: bool,
    undo_stack: Option<UndoStackRef>,
    cancelled: Arc<AtomicBool>,
) -> PersonaResult {
    let adapter = adapters::caching::maybe_wrap_cached(
        adapters::adapter_for(&model_name, &ollama_host, None, config.request_timeout_secs),
        &config,
    );
    let tools = tools_for_persona(kind, undo_stack.clone(), supports_images, &config);

    let conversation = match ConversationLog::open_async(fork_path.clone()).await {
        Ok((c, _outcome)) => c,
        Err(e) => {
            return PersonaResult {
                kind,
                task,
                fork_path,
                success: false,
                summary: String::new(),
                error: Some(format!("failed to open fork log: {e}")),
            }
        }
    };

    let shared_config: SharedConfig = Arc::new(std::sync::RwLock::new(config.clone()));
    let mut composite = CompositeToolset::empty();
    composite.add(Box::new(VecToolset::new("persona", tools)));
    let mut executor = Executor::with_log_and_undo(
        adapter,
        composite,
        shared_config,
        conversation,
        None,
        undo_stack.clone(),
    );

    // Explore reuses the executor-level plan-mode guard so bash is
    // restricted to read-only commands inside the fork.
    if kind == PersonaKind::Explore {
        executor.set_plan_mode(true);
    }

    // Auto-approve everything inside the isolated fork. The sandboxing,
    // read-only restrictions, and trust tiers are enforced by the toolset
    // and plan_mode instead.
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            crate::send_or_warn!(
                req.response.send(ApprovalResponse::Approved),
                "approval response receiver dropped; response discarded"
            );
        }
    });

    // `run_turn` is a single LLM turn that internally loops over tool
    // calls up to its own `MAX_ITERATIONS` cap. The `max_turns` config
    // is the high-level on/off guard for personas; future multi-turn
    // personas could loop here up to `max_turns` iterations.
    if max_turns == 0 {
        return PersonaResult {
            kind,
            task,
            fork_path,
            success: false,
            summary: String::new(),
            error: Some("max_persona_turns is 0; personas are disabled".into()),
        };
    }

    if cancelled.load(Ordering::SeqCst) {
        return PersonaResult {
            kind,
            task,
            fork_path,
            success: false,
            summary: String::new(),
            error: Some("persona cancelled".into()),
        };
    }

    let prompt = build_persona_prompt(kind, &task);
    if let Err(e) = executor
        .run_turn_collecting(&prompt, &approval_tx, &cancelled)
        .await
    {
        return PersonaResult {
            kind,
            task,
            fork_path,
            success: false,
            summary: String::new(),
            error: Some(format!("turn failed: {e}")),
        };
    }

    let summary = executor
        .conversation_log()
        .all()
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
        .map(|m| m.content.clone())
        .unwrap_or_else(|| "(no assistant response produced)".to_string());

    PersonaResult {
        kind,
        task,
        fork_path,
        success: true,
        summary,
        error: None,
    }
}

/// Start a persona run from the current session state.
///
/// Returns a user-visible status string and spawns a background task that
/// will send a `PersonaResult` through `completion_tx` when done.
pub async fn start_persona(
    kind: PersonaKind,
    args: &str,
    state: &mut AppState,
    completion_tx: mpsc::UnboundedSender<PersonaResult>,
) -> String {
    let task = args.trim();
    if task.is_empty() {
        return format!("Usage: /{kind} <task description> — start a fork-isolated {kind} persona");
    }

    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available (session not initialized).".into(),
    };

    let parent_log_path = match state.log_path.clone() {
        Some(p) => p,
        None => return "No session log path. Cannot fork for persona.".into(),
    };

    let parent_log = match ConversationLog::open_async(parent_log_path.clone()).await {
        Ok((l, _outcome)) => l,
        Err(e) => return format!("Cannot open session log: {e}"),
    };

    let fork_label = format!(
        "{}-{}",
        kind,
        task.split_whitespace().next().unwrap_or("task")
    );
    let fork = match fm.create_fork(&fork_label, &parent_log, -1) {
        Ok(f) => f,
        Err(e) => return format!("Failed to create fork: {e}"),
    };

    let fork_path = fork.path.clone();
    let fork_id = fork.id.clone();
    let task_owned = task.to_string();

    let cfg = read_shared_config(&state.config).clone();
    let max_turns = cfg.max_persona_turns;
    let model_name = state
        .model_info
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| cfg.default_model.clone());
    let ollama_host = cfg.ollama_host.clone();
    let supports_images = state
        .model_info
        .as_ref()
        .map(|m| m.supports_images)
        .unwrap_or(false);
    let undo_stack = state.undo_stack.clone();

    // Per-persona cancel flag. The TUI can set this from Ctrl+C when a
    // persona is running; the task polls it between internal turns.
    let cancelled = Arc::new(AtomicBool::new(false));
    state.persona_cancel = Some(cancelled.clone());

    tokio::spawn(async move {
        let result = run_persona_task(
            kind,
            task_owned,
            fork_path,
            max_turns,
            model_name,
            ollama_host,
            cfg,
            supports_images,
            undo_stack,
            cancelled,
        )
        .await;
        crate::send_or_warn!(
            completion_tx.send(result),
            "persona completion channel receiver dropped"
        );
    });

    state.persona_in_progress = Some(PersonaHandle {
        kind,
        task: task.to_string(),
        fork_id,
    });
    state.is_generating = true;

    format!("🚀 Started {kind} persona for: {task}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_persona_prompt_contains_task() {
        let p = build_persona_prompt(PersonaKind::Explore, "find auth code");
        assert!(p.contains("find auth code"));
        assert!(p.contains("exploratory research"));
    }

    #[test]
    fn test_build_plan_prompt_uses_marker() {
        let p = build_persona_prompt(PersonaKind::Plan, "add dark mode");
        assert!(p.contains("add dark mode"));
        assert!(p.contains("## Plan Complete"));
        assert!(p.contains("Do not run shell"));
    }

    #[test]
    fn test_build_coder_prompt_allows_edits() {
        let p = build_persona_prompt(PersonaKind::Coder, "fix bug");
        assert!(p.contains("fix bug"));
        assert!(p.contains("full toolset"));
    }

    #[test]
    fn test_tools_for_plan_excludes_bash() {
        let config = Config::default();
        let tools = tools_for_persona(PersonaKind::Plan, None, false, &config);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"grep"));
        assert!(!names.contains(&"bash"));
        assert!(!names.contains(&"edit_file"));
    }

    #[test]
    fn test_tools_for_explore_includes_bash() {
        let config = Config::default();
        let tools = tools_for_persona(PersonaKind::Explore, None, false, &config);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"bash"));
        assert!(!names.contains(&"edit_file"));
        assert!(!names.contains(&"write_file"));
    }

    #[test]
    fn test_tools_for_coder_has_all() {
        let config = Config::default();
        let tools = tools_for_persona(PersonaKind::Coder, None, false, &config);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"write_file"));
    }
}
