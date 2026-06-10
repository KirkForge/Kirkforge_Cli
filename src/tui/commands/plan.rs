//! `/plan` command — prompt-based plan mode.
//!
//! Sends a specially prefixed message to the model instructing it to act
//! as a software architect. The model is told to explore the codebase
//! (read-only tools), design an approach, and present a step-by-step plan
//! — no edits, no writes, no bash with side effects.
//!
//! # How it works
//!
//! The plan prompt wraps the user's task with a PLAN MODE prefix that
//! instructs the model to use only read-only discovery tools. The executor
//! still sees this as a regular user message — plan mode is purely a
//! prompt-engineering layer, not a separate execution path. This keeps
//! the executor simple while giving the model clear constraints.
//!
//! The `is_plan_prompt()` helper lets the TUI detect plan-mode messages
//! and mark them visually.

/// Build a plan-mode prompt from a user task description.
///
/// Returns the full prompt that should be sent to the executor.
pub fn build_plan_prompt(task: &str) -> String {
    format!(
        "PLAN MODE — You are a software architect. Your job is to explore \
         the codebase and design an implementation plan. DO NOT write any \
         code, edit files, or run destructive commands.\n\n\
         Rules:\n\
         - Use read_file, grep, and glob to understand the codebase\n\
         - Use bash only with read-only commands (ls, cat, git log, etc.)\n\
         - NEVER use write_file, edit_file, or bash with write/delete commands\n\
         - Produce a step-by-step plan with specific file paths\n\
         - List any risks, dependencies, or architectural decisions\n\
         - End with: \"## Plan Complete — ready to implement\"\n\n\
         Task: {}",
        task
    )
}

/// Handle the `/plan` slash command.
///
/// Returns `(display_message, plan_prompt)` where `display_message` goes
/// into the TUI message list and `plan_prompt` is sent to the executor.
pub fn handle_plan_command(args: &str) -> (String, String) {
    let task = args.trim();
    if task.is_empty() {
        return (
            "Usage: /plan <task description> — enter plan mode to explore \
             and design before implementing"
                .into(),
            String::new(),
        );
    }

    let plan_prompt = build_plan_prompt(task);
    let display = format!(
        "📐 Plan mode activated for: {}\n\n\
         The model will use read-only tools to explore the codebase \
         and design an implementation approach. No files will be modified.",
        task
    );

    (display, plan_prompt)
}

/// Check whether a user message is a plan-mode prompt.
pub fn is_plan_prompt(message: &str) -> bool {
    message.starts_with("PLAN MODE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_plan_prompt_wraps_task() {
        let prompt = build_plan_prompt("Add dark mode support");
        assert!(prompt.starts_with("PLAN MODE"));
        assert!(prompt.contains("Add dark mode support"));
        assert!(prompt.contains("DO NOT write any code"));
        assert!(prompt.contains("write_file"));
    }

    #[test]
    fn test_build_plan_prompt_includes_rules() {
        let prompt = build_plan_prompt("Refactor auth");
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("bash only with read-only"));
    }

    #[test]
    fn test_handle_plan_empty_returns_usage() {
        let (display, prompt) = handle_plan_command("");
        assert!(display.contains("Usage"));
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_handle_plan_whitespace_only_returns_usage() {
        let (display, prompt) = handle_plan_command("   ");
        assert!(display.contains("Usage"));
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_handle_plan_valid_task() {
        let (display, prompt) = handle_plan_command("Add OAuth");
        assert!(display.contains("📐"));
        assert!(display.contains("Add OAuth"));
        assert!(!prompt.is_empty());
        assert!(prompt.starts_with("PLAN MODE"));
    }

    #[test]
    fn test_is_plan_prompt() {
        assert!(is_plan_prompt("PLAN MODE — design auth"));
        assert!(!is_plan_prompt("Regular message"));
        assert!(!is_plan_prompt(""));
    }
}
