//! `/gh` command — GitHub operations via the `gh` CLI.
//!
//! Wraps `gh issue`, `gh pr`, `gh search`, `gh run`, and `gh api` to
//! provide GitHub operations from within the TUI. All subcommands
//! delegate to the `gh` CLI and format output for display.
//!
//! # Subcommands
//!
//! - `issue list [repo] [--label X]` — list issues
//! - `issue view <number>` — view issue details
//! - `pr list [repo]` — list pull requests
//! - `pr view <number>` — view PR details
//! - `pr diff <number>` — view PR diff
//! - `search <query>` — search code
//! - `run list` — list workflow runs
//! - `run view <id>` — view workflow run details
//! - `file <path> [ref]` — view file content

use std::process::Command;

/// Handle `/gh <subcommand> [args...]` — returns formatted output for display.
pub fn handle_gh_command(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return usage();
    }

    let mut parts = args.splitn(2, ' ');
    let subcommand = parts.next().unwrap_or("");
    let sub_args = parts.next().unwrap_or("");

    match subcommand {
        "issue" => handle_issue(sub_args),
        "pr" => handle_pr(sub_args),
        "search" => handle_search(sub_args),
        "run" => handle_run(sub_args),
        "file" => handle_file(sub_args),
        _ => format!(
            "Unknown subcommand: /gh {}\n\n{}",
            subcommand,
            usage()
        ),
    }
}

fn usage() -> String {
    "GitHub commands:\n\
     /gh issue list [repo] [--label X]\n\
     /gh issue view <number>\n\
     /gh pr list [repo]\n\
     /gh pr view <number>\n\
     /gh pr diff <number>\n\
     /gh search <query>\n\
     /gh run list\n\
     /gh run view <id>\n\
     /gh file <path> [ref]"
        .to_string()
}

// --- issue subcommands ---

fn handle_issue(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return "/gh issue list | view <number>\n  list [repo] [--label X]\n  view <number>".into();
    }

    if args.starts_with("list") {
        let rest = args.strip_prefix("list").unwrap_or("").trim();
        match run_gh_issue_list(rest) {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args.starts_with("view ") {
        let number = args.strip_prefix("view ").unwrap_or("").trim();
        match run_gh_issue_view(number) {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args == "view" {
        "/gh issue view <number> — e.g. /gh issue view 42".into()
    } else if args == "create" {
        "Creating issues requires an interactive editor. Run `gh issue create` in a terminal.".into()
    } else {
        format!("Unknown /gh issue subcommand: {}.\nUse: list | view <number>", args)
    }
}

fn run_gh_issue_list(args: &str) -> Result<String, String> {
    let label = extract_label_flag(args);
    let mut cmd = Command::new("gh");
    cmd.args(["issue", "list", "--json", "number,title,state,labels", "--limit", "20"]);
    if let Some(l) = &label {
        cmd.args(["--label", l]);
    }
    exec_gh(cmd, "gh issue list")
}

fn run_gh_issue_view(number: &str) -> Result<String, String> {
    if number.is_empty() || number.parse::<u64>().is_err() {
        return Err("Invalid issue number. Example: /gh issue view 42".into());
    }
    let mut cmd = Command::new("gh");
    cmd.args(["issue", "view", number]);
    exec_gh(cmd, "gh issue view")
}

// --- pr subcommands ---

fn handle_pr(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return "/gh pr list | view <number> | diff <number>\n  list [repo]\n  view <number>\n  diff <number>".into();
    }

    if args.starts_with("list") {
        match run_gh_pr_list() {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args.starts_with("diff ") {
        let number = args.strip_prefix("diff ").unwrap_or("").trim();
        match run_gh_pr_diff(number) {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args.starts_with("view ") {
        let number = args.strip_prefix("view ").unwrap_or("").trim();
        match run_gh_pr_view(number) {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args == "view" || args == "diff" {
        format!("/gh pr {} <number> — e.g. /gh pr {} 42", args, args)
    } else {
        format!("Unknown /gh pr subcommand: {}.\nUse: list | view <number> | diff <number>", args)
    }
}

fn run_gh_pr_list() -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "list", "--json", "number,title,state,author,headRefName,baseRefName", "--limit", "10"]);
    exec_gh(cmd, "gh pr list")
}

fn run_gh_pr_view(number: &str) -> Result<String, String> {
    if number.is_empty() || number.parse::<u64>().is_err() {
        return Err("Invalid PR number. Example: /gh pr view 42".into());
    }
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "view", number]);
    exec_gh(cmd, "gh pr view")
}

fn run_gh_pr_diff(number: &str) -> Result<String, String> {
    if number.is_empty() || number.parse::<u64>().is_err() {
        return Err("Invalid PR number. Example: /gh pr diff 42".into());
    }
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "diff", number]);
    exec_gh(cmd, "gh pr diff")
}

// --- search ---

fn handle_search(args: &str) -> String {
    let query = args.trim();
    if query.is_empty() {
        return "/gh search <query> — search GitHub code. Example: /gh search rand crate".into();
    }
    match run_gh_search(query) {
        Ok(out) => out,
        Err(e) => e,
    }
}

fn run_gh_search(query: &str) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(["search", "code", query, "--limit", "15"]);
    exec_gh(cmd, "gh search code")
}

// --- workflow runs ---

fn handle_run(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return "/gh run list | view <id>".into();
    }

    if args == "list" {
        match run_gh_run_list() {
            Ok(out) => out,
            Err(e) => e,
        }
    } else if args.starts_with("view ") {
        let id = args.strip_prefix("view ").unwrap_or("").trim();
        match run_gh_run_view(id) {
            Ok(out) => out,
            Err(e) => e,
        }
    } else {
        format!("Unknown /gh run subcommand: {}.\nUse: list | view <id>", args)
    }
}

fn run_gh_run_list() -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(["run", "list", "--limit", "10"]);
    exec_gh(cmd, "gh run list")
}

fn run_gh_run_view(id: &str) -> Result<String, String> {
    if id.is_empty() {
        return Err("Run ID required. Example: /gh run view 1234567890".into());
    }
    let mut cmd = Command::new("gh");
    cmd.args(["run", "view", id]);
    exec_gh(cmd, "gh run view")
}

// --- file content ---

fn handle_file(args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return "/gh file <path> [ref] — view file content from the default branch.\n\
                Example: /gh file src/main.rs main".into();
    }
    let path = parts[0];
    let ref_name = parts.get(1).copied().unwrap_or("HEAD");
    match run_gh_file(path, ref_name) {
        Ok(out) => out,
        Err(e) => e,
    }
}

fn run_gh_file(path: &str, ref_name: &str) -> Result<String, String> {
    // gh api returns base64-encoded content. Pipe through bash `base64 -d`
    // to avoid pulling in a base64 crate dependency.
    let cmd_str = format!(
        "gh api 'repos/:owner/:repo/contents/{}?ref={}' --jq '.content' | base64 -d 2>/dev/null",
        path, ref_name
    );
    let output = Command::new("bash")
        .args(["-c", &cmd_str])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "gh CLI not found. Install it: https://cli.github.com/".into()
            } else {
                format!("Failed to run gh file: {}", e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh file failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// --- helpers ---

/// Run a `gh` command and return stdout or a formatted error.
fn exec_gh(mut cmd: Command, label: &str) -> Result<String, String> {
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "gh CLI not found. Install it: https://cli.github.com/".into()
        } else {
            format!("Failed to run {}: {}", label, e)
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let err_msg = stderr.trim().to_string();
        if err_msg.is_empty() {
            return Err(format!("{} exited with status {}", label, output.status));
        }
        // Common error: not authenticated
        if err_msg.contains("not authenticated") || err_msg.contains("auth") {
            return Err("gh not authenticated. Run `gh auth login` in a terminal.".into());
        }
        return Err(format!("{} failed: {}", label, err_msg));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        Ok("(no output)".into())
    } else {
        Ok(stdout)
    }
}

/// Extract `--label <value>` from args string.
fn extract_label_flag(args: &str) -> Option<String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    for i in 0..parts.len().saturating_sub(1) {
        if parts[i] == "--label" {
            return Some(parts[i + 1].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_not_empty() {
        assert!(!usage().is_empty());
        assert!(usage().contains("issue"));
        assert!(usage().contains("pr"));
    }

    #[test]
    fn test_handle_gh_empty_returns_usage() {
        let out = handle_gh_command("");
        assert!(out.contains("GitHub commands"), "got: {}", out);
    }

    #[test]
    fn test_handle_gh_unknown_subcommand() {
        let out = handle_gh_command("nope");
        assert!(out.contains("Unknown"), "got: {}", out);
    }

    #[test]
    fn test_issue_empty_returns_help() {
        let out = handle_issue("");
        assert!(out.contains("list"), "got: {}", out);
        assert!(out.contains("view"), "got: {}", out);
    }

    #[test]
    fn test_issue_view_no_number() {
        let out = handle_issue("view");
        assert!(out.contains("42"), "got: {}", out);
    }

    #[test]
    fn test_issue_create_returns_instructions() {
        let out = handle_issue("create");
        assert!(out.contains("interactive"), "got: {}", out);
    }

    #[test]
    fn test_pr_empty_returns_help() {
        let out = handle_pr("");
        assert!(out.contains("list"), "got: {}", out);
        assert!(out.contains("diff"), "got: {}", out);
    }

    #[test]
    fn test_pr_view_empty_number() {
        let out = handle_pr("view");
        assert!(out.contains("42"), "got: {}", out);
    }

    #[test]
    fn test_pr_diff_empty_number() {
        let out = handle_pr("diff");
        assert!(out.contains("42"), "got: {}", out);
    }

    #[test]
    fn test_search_empty_returns_help() {
        let out = handle_search("");
        assert!(out.contains("search GitHub"), "got: {}", out);
    }

    #[test]
    fn test_run_empty_returns_help() {
        let out = handle_run("");
        assert!(out.contains("list"), "got: {}", out);
    }

    #[test]
    fn test_run_view_empty_id() {
        let out = handle_run("view");
        // Should get help since "view" alone isn't valid
        assert!(out.contains("list"), "got: {}", out);
    }

    #[test]
    fn test_file_empty_returns_help() {
        let out = handle_file("");
        assert!(out.contains("path"), "got: {}", out);
    }

    #[test]
    fn test_extract_label_flag_present() {
        assert_eq!(
            extract_label_flag("--label bug"),
            Some("bug".to_string())
        );
    }

    #[test]
    fn test_extract_label_flag_missing() {
        assert_eq!(extract_label_flag("no labels here"), None);
    }

    #[test]
    fn test_extract_label_flag_multiple() {
        assert_eq!(
            extract_label_flag("--label bug --label feature"),
            Some("bug".to_string()) // first match only
        );
    }
}
