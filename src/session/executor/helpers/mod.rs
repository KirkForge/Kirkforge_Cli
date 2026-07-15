//! Free helper functions used by the executor.
//!
//! Tool-outcome processing (`handle_tool_outcome`, `emit_correction_results`)
//! lives in [`outcome`]; it is re-exported here so `use super::helpers::*;`
//! in `dispatch.rs` keeps working unchanged.

mod outcome;

pub(crate) use outcome::{emit_correction_results, handle_tool_outcome};

use crate::session::access::{url_is_denied, DenyList, GuardVerdict, PathGuard};
use crate::shared::metrics::{record, MetricEvent};
use crate::shared::ToolOutcome;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub(crate) fn tool_outcome_success(outcome: &ToolOutcome) -> bool {
    !matches!(
        outcome,
        ToolOutcome::Error { .. } | ToolOutcome::Failure { .. }
    )
}

pub(crate) fn tool_error_kind(outcome: &ToolOutcome) -> Option<&'static str> {
    match outcome {
        ToolOutcome::Error { .. } => Some("error"),
        ToolOutcome::Failure(err) => match err {
            crate::shared::ToolError::InvalidArgs { .. } => Some("invalid_args"),
            crate::shared::ToolError::AccessDenied { .. } => Some("access_denied"),
            crate::shared::ToolError::Execution { .. } => Some("execution"),
            crate::shared::ToolError::Timeout { .. } => Some("timeout"),
            crate::shared::ToolError::Cancelled => Some("cancelled"),
            crate::shared::ToolError::Internal { .. } => Some("internal"),
        },
        _ => None,
    }
}

pub(crate) fn record_turn_metric(
    model: &str,
    start: Instant,
    tool_calls: usize,
    finish_reason: &crate::shared::FinishReason,
) {
    record(MetricEvent::Turn {
        model: model.to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        tool_calls,
        finish_reason: format!("{finish_reason:?}").to_lowercase(),
    });
}
pub(crate) fn tool_cancel_token(
    cancelled: &std::sync::atomic::AtomicBool,
) -> tokio_util::sync::CancellationToken {
    let token = tokio_util::sync::CancellationToken::new();
    if cancelled.load(Ordering::SeqCst) {
        token.cancel();
    }
    token
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "which", "type", "file", "stat", "du",
    "df", "env", "printenv", "true", "false", "dirname", "basename", "realpath", "readlink",
    "grep", "rg", "sort", "wc", "cut", "tr", "uniq", "fold", "nl", "diff", "cmp", "comm", "jq",
    "date", "cal", "whoami", "id", "uname", "hostname", "uptime", "ps", "free", "lscpu", "lsblk",
    "lsof", "dmesg", "nproc", "arch", "tty", "jobs", "help", "find", "git",
];

/// Git subcommands that are inherently read-only.  The model commonly asks for
/// `git status`, `git log`, `git diff`, and `git show`; allowing these avoids
/// constant approval prompts while still requiring explicit approval for any
/// mutating subcommand (`add`, `commit`, `push`, `checkout`, `reset`, ...).
const READ_ONLY_GIT_SUBCOMMANDS: &[&str] =
    &["status", "log", "diff", "show", "ls-files", "rev-parse"];

/// Return true if the git command appears to be a read-only query.
///
/// This parses the command line just enough to skip global options (`-C`,
/// `--no-pager`, etc.) and look at the actual subcommand.  It rejects empty
/// or unknown subcommands so a bare `git` or `git add` does not auto-approve.
fn git_command_is_read_only(cmd: &str) -> bool {
    let mut tokens = cmd.split_whitespace().skip(1).peekable();
    let subcommand = loop {
        match tokens.next() {
            Some(tok) if tok.starts_with('-') => {
                // Global options that consume a following argument.
                if tok == "-C" {
                    tokens.next();
                }
                continue;
            }
            Some(tok) => break tok,
            None => return false,
        }
    };
    READ_ONLY_GIT_SUBCOMMANDS.contains(&subcommand)
}

pub(crate) fn is_read_only_bash(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }

    let trimmed_stripped = trimmed.trim_start();
    let first = match trimmed_stripped.find(|c: char| c.is_whitespace()) {
        Some(pos) => &trimmed_stripped[..pos],
        None => trimmed_stripped,
    };

    if !READ_ONLY_COMMANDS.contains(&first) {
        return false;
    }

    // Every pipe segment must itself be a read-only command, and no segment
    // may contain shell metacharacters that could escape the read-only guard.
    // Without this, a pipeline such as `cat list | sort > /tmp/out` or
    // `cat list | sort; rm -rf /` would be auto-approved despite mutating
    // state or executing arbitrary commands.
    //
    // Command-specific guards (`find` destructive flags, `git` subcommand
    // allowlist) are applied to each segment so a read-only producer cannot
    // hide a mutating consumer later in the pipeline.
    for segment in trimmed.split('|') {
        let seg = segment.trim();
        let word = match seg.split_whitespace().next() {
            Some(w) => w,
            None => return false, // malformed pipe segment
        };

        if !READ_ONLY_COMMANDS.contains(&word) {
            return false;
        }

        if word == "find" {
            let lowered = seg.to_lowercase();
            for flag in [
                " -delete",
                " -exec",
                " -execdir",
                " -ok",
                " -okdir",
                " -fprint",
                " -fls",
            ] {
                if lowered.contains(flag) {
                    return false;
                }
            }
        }

        if word == "git" && !git_command_is_read_only(seg) {
            return false;
        }

        if seg.contains('>')
            || seg.contains(';')
            || seg.contains("&&")
            || seg.contains("||")
            || seg.contains("$(")
            || seg.contains('`')
        {
            return false;
        }
    }

    true
}

pub(crate) fn truncate_tool_output(outcome: ToolOutcome, max_chars: usize) -> ToolOutcome {
    if max_chars == 0 {
        return outcome;
    }
    match outcome {
        ToolOutcome::Success { content } => {
            if content.len() > max_chars {
                let mut boundary = max_chars;
                while !content.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                let truncated = format!(
                    "{}...\n[output truncated to {} chars]",
                    &content[..boundary],
                    max_chars
                );
                ToolOutcome::Success { content: truncated }
            } else {
                ToolOutcome::Success { content }
            }
        }
        ToolOutcome::Error { message } => ToolOutcome::Error { message },
        ToolOutcome::Failure(err) => ToolOutcome::Failure(err.clone()),
        other => other,
    }
}

pub(crate) fn extract_bash_metrics(
    outcome: &ToolOutcome,
) -> (Option<i32>, Option<usize>, Option<usize>) {
    match outcome {
        ToolOutcome::Success { content } => (Some(0), Some(content.len()), Some(0)),
        ToolOutcome::Error { message } => {
            let exit_code = if message.contains("exited with code") {
                message
                    .split("exited with code ")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<i32>().ok())
            } else {
                Some(1)
            };
            let (stdout_len, stderr_len) = message
                .find("\nstderr:\n")
                .map(|pos| (pos, message[pos + 9..].len()))
                .unwrap_or((message.len(), 0));
            (exit_code, Some(stdout_len), Some(stderr_len))
        }
        ToolOutcome::Failure(crate::shared::ToolError::Execution {
            exit_code, stderr, ..
        }) => (*exit_code, Some(0), Some(stderr.len())),
        ToolOutcome::Failure(crate::shared::ToolError::Timeout { .. })
        | ToolOutcome::Failure(crate::shared::ToolError::Cancelled) => (Some(1), Some(0), Some(0)),
        ToolOutcome::Failure(_) => (Some(1), Some(0), Some(0)),
        _ => (None, None, None),
    }
}

/// PathGuard-style check for grep/glob search paths.
///
/// `PathGuard::check_read` requires the path to exist, but grep/glob
/// arguments are often glob patterns (`src/**/*.rs`) or directories
/// that may not exist yet. This helper does the deny-list and sandbox
/// containment checks without requiring existence, falling back to
/// the longest existing ancestor for containment.
///
/// This was the source of GPT 5.5's review finding #3 ("PathGuard
/// applied to grep/glob") — without this, a model could enumerate
/// files outside the sandbox via grep/glob even though
/// read/write/edit were guarded.
pub(crate) fn check_search_path(path_guard: &PathGuard, path: &std::path::Path) -> GuardVerdict {
    // 1. Deny list — same as check_read.
    if path_guard.deny_list.is_path_denied(path) {
        return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
    }

    // 2. Resolve to the longest existing ancestor so glob patterns
    //    still get a containment check. Fail closed if we cannot resolve
    //    the path or any existing ancestor — a non-canonical path could
    //    pass a naive prefix check while resolving outside the sandbox.
    let check = if path.exists() {
        match path.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve search path '{}': {e} (refusing unverified search)",
                    path.display()
                ));
            }
        }
    } else {
        let mut cur = path.to_path_buf();
        while !cur.exists() {
            if !cur.pop() {
                break;
            }
        }
        match cur.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve search path '{}': {e} (refusing unverified search)",
                    path.display()
                ));
            }
        }
    };

    // 3b. Re-check the resolved path against the deny list. A glob/pattern
    //     may resolve to a denied directory, or a symlink may point to one.
    if path_guard.deny_list.is_path_denied(&check) {
        return GuardVerdict::Denied(format!(
            "Resolved path denied by deny list: {}",
            check.display()
        ));
    }

    // 3. Sandbox containment on the resolved ancestor.
    if let Some(ref sandbox) = path_guard.sandbox_dir {
        let sb = match sandbox.canonicalize() {
            Ok(s) => s,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve sandbox dir '{}': {e}",
                    sandbox.display()
                ));
            }
        };
        if !check.starts_with(&sb) {
            return GuardVerdict::Denied(format!(
                "Search path outside sandbox: {}",
                path.display()
            ));
        }
    }

    GuardVerdict::Allowed(path.to_path_buf())
}

pub(crate) fn check_deny_list(
    deny_list: &DenyList,
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    match tool_name {
        "read_file" | "read_image" | "write_file" | "edit_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        "bash" => {}
        "grep" | "glob" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        _ => {}
    }
    None
}

/// Pre-flight URL deny-list check for tool arguments.
///
/// Scans the common URL argument keys (`url`, `endpoint`, `api_url`) and
/// returns a denial message if any value starts with a blocked prefix. This
/// is defensive plumbing: no built-in tool currently fetches URLs, but MCP
/// or plugin tools may expose a `url` parameter in the future.
pub(crate) fn check_url_in_args(args: &serde_json::Value, deny_list: &DenyList) -> Option<String> {
    if let Some(obj) = args.as_object() {
        for key in ["url", "endpoint", "api_url"] {
            if let Some(url) = obj.get(key).and_then(|v| v.as_str()) {
                if url_is_denied(url, &deny_list.url_patterns) {
                    return Some(format!("🔒 URL denied by deny list: {url}"));
                }
            }
        }
    }
    None
}

/// Lightweight pre-flight validation of tool arguments against a JSON Schema
/// fragment. The `required` array, per-property `type`, `anyOf`, and `oneOf`
/// fields are checked; this is enough to catch the most common model failures
/// (missing a required parameter, passing a string where an integer is
/// expected, passing a string where either a string or an array is acceptable)
/// before handing the call to a plugin script or MCP server.
///
/// Returns `None` when the arguments look valid, or `Some(reason)` when they
/// do not.
pub(crate) fn validate_args_against_schema(
    args: &serde_json::Value,
    schema: &serde_json::Value,
) -> Option<String> {
    let properties = schema.get("properties").and_then(|p| p.as_object())?;
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str())
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();

    let args_obj = match args.as_object() {
        Some(o) => o,
        None => return Some(format!("arguments must be a JSON object, got {args}")),
    };

    for req in &required {
        if !args_obj.contains_key(*req) {
            return Some(format!("missing required argument '{req}'"));
        }
    }

    for (key, value) in args_obj {
        let prop_schema = match properties.get(key) {
            Some(s) => s,
            None => continue, // unknown keys are allowed; the tool can ignore them
        };
        if let Some(reason) = value_matches_schema(value, prop_schema, key) {
            return Some(reason);
        }
    }

    None
}

/// Returns `None` if `value` satisfies `schema`, or `Some(reason)` otherwise.
/// Supports `type`, `anyOf`, `oneOf`, and array `items`.
fn value_matches_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    key: &str,
) -> Option<String> {
    // JSON Schema combinators: the value is valid if it matches at least one
    // of the listed sub-schemas. We treat `oneOf` the same as `anyOf` for this
    // lightweight validator; the goal is to allow polymorphic fields (e.g.
    // "string or array of strings") without accidentally rejecting valid calls.
    for combinator in ["anyOf", "oneOf"] {
        if let Some(alternatives) = schema.get(combinator).and_then(|a| a.as_array()) {
            if alternatives.is_empty() {
                return None;
            }
            let mut reasons = Vec::new();
            for alt in alternatives {
                if let Some(reason) = value_matches_schema(value, alt, key) {
                    reasons.push(reason);
                } else {
                    return None;
                }
            }
            return Some(format!(
                "argument '{key}' did not match any of the {combinator} schemas ({})",
                reasons.join("; ")
            ));
        }
    }

    let expected = schema.get("type").and_then(|t| t.as_str());
    if let Some(expected) = expected {
        if let Some(reason) = value_matches_type(value, expected, key) {
            return Some(reason);
        }
        // Validate array item types if declared.
        if expected == "array" {
            if let Some(items) = schema.get("items") {
                if let Some(arr) = value.as_array() {
                    for (idx, item) in arr.iter().enumerate() {
                        if let Some(reason) = value_matches_schema(item, items, key) {
                            return Some(format!(
                                "array item {idx} for '{key}' has wrong type: {reason}"
                            ));
                        }
                    }
                }
            }
        }
    }

    None
}

fn value_matches_type(value: &serde_json::Value, expected: &str, key: &str) -> Option<String> {
    let ok = match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true, // unknown type in schema is ignored
    };
    if ok {
        None
    } else {
        Some(format!(
            "argument '{key}' expected type '{expected}', got {}",
            json_value_type_name(value)
        ))
    }
}

fn json_value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
        serde_json::Value::Null => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_search_path_fails_closed_on_unresolvable_path() {
        // A relative path with no existing components cannot be resolved,
        // so the function must deny rather than falling back to the literal
        // path (which could accidentally pass a sandbox prefix check).
        let dir = std::env::temp_dir().join("kirkforge_search_unresolvable_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();

        let guard = crate::session::access::PathGuard {
            sandbox_dir: Some(dir.clone()),
            ..Default::default()
        };

        let result = check_search_path(
            &guard,
            std::path::Path::new("totally_nonexistent_dir/search"),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("Cannot resolve search path")),
            "expected fail-closed denial, got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_check_search_path_rechecks_deny_list_on_canonical_target() {
        let dir = std::env::temp_dir().join("kirkforge_search_deny_symlink_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();

        let target = dir.join("secret.pem");
        let link = dir.join("safe_link");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let guard = crate::session::access::PathGuard {
            sandbox_dir: Some(dir.clone()),
            ..Default::default()
        };

        // The symlink name is not denied, but its canonical target is *.pem.
        let result = check_search_path(&guard, &link);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("secret.pem")),
            "expected search-path denial of symlink target, got {result:?}"
        );
    }

    #[test]
    fn test_check_url_in_args_blocks_denied_url() {
        let mut deny_list = DenyList::default();
        deny_list
            .url_patterns
            .push("https://internal.example.com".into());
        let args = serde_json::json!({"url": "https://internal.example.com/secrets"});
        let result = check_url_in_args(&args, &deny_list);
        assert!(
            result.as_ref().is_some_and(|m| m.contains("URL denied")),
            "denied url argument should be blocked, got: {result:?}"
        );
    }

    #[test]
    fn test_check_url_in_args_allows_safe_url() {
        let deny_list = DenyList::default();
        let args = serde_json::json!({"url": "https://api.example.com/data"});
        assert!(
            check_url_in_args(&args, &deny_list).is_none(),
            "safe url argument should be allowed"
        );
    }

    #[test]
    fn test_check_url_in_args_checks_endpoint_and_api_url() {
        let mut deny_list = DenyList::default();
        deny_list.url_patterns.push("http://100.100.100.200".into());
        for key in ["endpoint", "api_url"] {
            let args = serde_json::json!({key: "http://100.100.100.200/latest/meta-data/"});
            assert!(
                check_url_in_args(&args, &deny_list).is_some(),
                "denied {key} argument should be blocked"
            );
        }
    }

    #[test]
    fn test_validate_args_missing_required() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "workspace": { "type": "string" } },
            "required": ["workspace"]
        });
        let args = serde_json::json!({});
        assert!(
            validate_args_against_schema(&args, &schema)
                .is_some_and(|m| m.contains("missing required argument 'workspace'")),
            "missing required arg should be rejected"
        );
    }

    #[test]
    fn test_validate_args_wrong_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "timeout": { "type": "integer" } }
        });
        let args = serde_json::json!({"timeout": "thirty"});
        assert!(
            validate_args_against_schema(&args, &schema)
                .is_some_and(|m| m.contains("expected type 'integer'")),
            "wrong type should be rejected, got: {:?}",
            validate_args_against_schema(&args, &schema)
        );
    }

    #[test]
    fn test_validate_args_array_item_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "file": { "type": "array", "items": { "type": "string" } }
            }
        });
        let args = serde_json::json!({"file": ["a.txt", 1]});
        assert!(
            validate_args_against_schema(&args, &schema)
                .is_some_and(|m| m.contains("array item 1")),
            "array item with wrong type should be rejected"
        );
    }

    #[test]
    fn test_validate_args_allows_valid_call() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "workspace": { "type": "string" },
                "timeout": { "type": "integer" }
            },
            "required": ["workspace"]
        });
        let args = serde_json::json!({"workspace": "/tmp/ws", "timeout": 30});
        assert!(
            validate_args_against_schema(&args, &schema).is_none(),
            "valid args should pass validation"
        );
    }

    #[test]
    fn test_validate_args_anyof_accepts_string_or_array() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                }
            }
        });
        assert!(
            validate_args_against_schema(&serde_json::json!({"file": "single.txt"}), &schema)
                .is_none(),
            "single string should match anyOf"
        );
        assert!(
            validate_args_against_schema(&serde_json::json!({"file": ["a.txt", "b.txt"]}), &schema)
                .is_none(),
            "string array should match anyOf"
        );
        assert!(
            validate_args_against_schema(&serde_json::json!({"file": 42}), &schema)
                .is_some_and(|m| m.contains("did not match any of the anyOf schemas")),
            "wrong type should be rejected by anyOf"
        );
    }
}
