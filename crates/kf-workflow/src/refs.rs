use std::collections::HashMap;

use crate::StepOutput;
use anyhow::Result;

/// Resolve `$(step_name)` and `$(step_name.field.path)` references in a string
/// using completed step outputs. `$(step_name)` is replaced with the step's
/// summary. `$(step_name.field)` extracts `field` (and nested paths via `.`)
/// from the step's structured output; falls back to the summary if no
/// structured output is available. Unknown step names are left as-is.
///
/// The `$(...)` reference syntax uses ASCII `$`, `(`, `)` so byte-level
/// matching is correct for references. Surrounding text is emitted
/// character-by-character so multi-byte UTF-8 is preserved.
pub fn resolve_step_refs(text: &str, outputs: &HashMap<String, StepOutput>) -> String {
    let mut result = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut char_indices = text.char_indices().peekable();
    while let Some((byte_pos, ch)) = char_indices.next() {
        if ch == '$' && char_indices.peek().is_some_and(|(_, c)| *c == '(') {
            // Find the closing ')' using byte offsets (all reference
            // chars are ASCII, so byte scanning is correct here).
            let start = byte_pos + 2; // skip past "$("
            let mut depth = 1;
            let mut end = start;
            while end < bytes.len() && depth > 0 {
                if bytes[end] == b'(' {
                    depth += 1;
                } else if bytes[end] == b')' {
                    depth -= 1;
                }
                if depth > 0 {
                    end += 1;
                }
            }
            if depth != 0 {
                // Unmatched paren — leave as-is.
                result.push('$');
                continue;
            }
            let content = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
            // Split into step_name.field.path
            let mut parts = content.splitn(2, '.');
            let step_name = parts.next().unwrap_or("");
            let field_path = parts.next();
            if let Some(output) = outputs.get(step_name) {
                if let Some(path) = field_path {
                    if let Some(ref val) = output.structured_output {
                        result.push_str(&extract_field(val, path));
                    } else {
                        result.push_str(&output.summary);
                    }
                } else {
                    result.push_str(&output.summary);
                }
            } else {
                // Unknown step — leave as-is.
                result.push_str(&format!("$({content})"));
            }
            // Advance the character iterator past the reference.
            while let Some((pos, _)) = char_indices.peek() {
                if *pos <= end {
                    char_indices.next();
                } else {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Extract a dotted field path from a JSON value, returning the string
/// representation. Strings and numbers are unwrapped; everything else is
/// JSON-serialized. Numeric path components index into arrays.
fn extract_field(val: &serde_json::Value, path: &str) -> String {
    let mut current = val;
    for field in path.split('.') {
        // Try string key first (for objects), then numeric index (for arrays).
        let next = current
            .get(field)
            .or_else(|| field.parse::<usize>().ok().and_then(|i| current.get(i)));
        match next {
            Some(v) => current = v,
            None => return format!("$({path} not found)"),
        }
    }
    match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Wall-clock bound for condition evaluation. Mirrors the workflow bash
/// step timeout (`WORKFLOW_BASH_TIMEOUT_SECS` in `src/tools/workflow.rs`):
/// a condition should be a quick test/grep, not a long-running command.
/// `ceiling:` a template that needs a longer condition should surface a
/// per-step timeout knob; upgrade path is a `step.timeout_secs` field.
#[cfg(not(test))]
const CONDITION_TIMEOUT_SECS: u64 = 30;
/// Under cfg(test) the bound is 2s so the `sleep infinity` pinning test
/// resolves fast instead of hanging the suite for 30s.
#[cfg(test)]
const CONDITION_TIMEOUT_SECS: u64 = 2;

/// Character allowlist for condition strings. Rejects shell
/// metacharacters that enable command injection (semicolon, ampersand,
/// pipe, dollar, backtick, and newline-as-separator are all outside the
/// set), so a user-editable workflow JSON cannot smuggle an arbitrary
/// command past `sh -c`.
/// ponytail: char allowlist, not a parser — the allowed charset can
/// still build odd test expressions, but cannot inject commands.
/// Ceiling: upgrade to a real expression parser if conditions need
/// richer logic.
fn is_safe_condition_char(c: char) -> bool {
    matches!(
        c,
        'a'..='z'
            | 'A'..='Z'
            | '0'..='9'
            | ' '
            | '_'
            | '-'
            | '='
            | '!'
            | '<'
            | '>'
            | '('
            | ')'
            | '"'
            | '\''
            | '\t'
            | '\n'
            | '/'
            | '.'
    )
}

/// Evaluate a shell condition string with a wall-clock bound and
/// `kill_on_drop`. Returns `Ok(true)` if the condition exits 0,
/// `Ok(false)` on timeout (a hung condition skips the step, not the
/// workflow), and `Err` on spawn failure — the caller surfaces a spawn
/// failure as a workflow error instead of silently skipping the step.
///
/// `prepare` is an optional pre-spawn hook applied to the `sh -c`
/// `Command` before spawn (WO 47.25): the bin side passes the same
/// landlock+rlimit pre_exec the foreground bash tool gets, so condition
/// evals and bash steps share one sandboxed spawn path instead of two
/// with diverging guarantees. `None` spawns bare (crate default, tests).
/// ponytail: sh -c eval — upgrade to expression parser if needed.
pub async fn eval_condition_bounded(
    condition: &str,
    prepare: Option<&(dyn Fn(&mut tokio::process::Command) + Send + Sync)>,
) -> Result<bool> {
    if let Some(bad) = condition.chars().find(|c| !is_safe_condition_char(*c)) {
        tracing::warn!("rejecting condition with unsafe char {bad:?}: {condition}");
        return Err(anyhow::anyhow!(
            "condition contains disallowed character {bad:?}: {condition}"
        ));
    }
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(condition)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(prep) = prepare {
        prep(&mut cmd);
    }
    let status_fut = cmd.status();
    let timeout_fut = tokio::time::sleep(std::time::Duration::from_secs(CONDITION_TIMEOUT_SECS));
    tokio::pin!(status_fut);
    tokio::pin!(timeout_fut);
    tokio::select! {
        biased;
        result = &mut status_fut => match result {
            Ok(status) => Ok(status.success()),
            Err(e) => {
                tracing::warn!("condition eval failed for '{condition}': {e}");
                Err(anyhow::anyhow!(
                    "condition eval failed for '{condition}': {e}"
                ))
            }
        },
        _ = &mut timeout_fut => {
            tracing::warn!(
                "condition '{condition}' timed out after {CONDITION_TIMEOUT_SECS}s — skipping step"
            );
            Ok(false)
        }
    }
    // On timeout the `cmd` future (still owned here) is dropped; `kill_on_drop`
    // reaps the spawned `sh` process so a `sleep infinity` condition cannot
    // outlive the bound.
}

#[cfg(test)]
mod tests {
    use super::*;

    // WO 50.09: a clean condition with only allowlisted chars passes the
    // guard and reaches the shell (it evaluates true here).
    #[tokio::test]
    async fn eval_condition_clean_condition_passes_guard() {
        assert!(eval_condition_bounded("test 1 -eq 1", None).await.unwrap());
    }

    // WO 50.09: a command-injection attempt via `;` is rejected before
    // spawn — `rm -rf /` must never run.
    #[tokio::test]
    async fn eval_condition_rejects_semicolon_injection() {
        let err = eval_condition_bounded("true; rm -rf /", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("disallowed character"),
            "expected disallowed-char error, got: {err}"
        );
    }

    // WO 50.09: command substitution `$()` is rejected — `$` is outside
    // the allowlist.
    #[tokio::test]
    async fn eval_condition_rejects_command_substitution() {
        let err = eval_condition_bounded("test $(whoami) = root", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("disallowed character"),
            "expected disallowed-char error, got: {err}"
        );
    }

    // WO 50.09: backtick command substitution is rejected — backtick is
    // outside the allowlist.
    #[tokio::test]
    async fn eval_condition_rejects_backtick_injection() {
        let err = eval_condition_bounded("test `whoami` = root", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("disallowed character"),
            "expected disallowed-char error, got: {err}"
        );
    }
}
