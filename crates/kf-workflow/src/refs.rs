use std::collections::HashMap;

use crate::StepOutput;

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

/// Evaluate a shell condition string with a wall-clock bound and
/// `kill_on_drop`. Returns `true` if the condition exits 0, `false`
/// otherwise — including timeout and spawn failure, so a hung condition
/// skips the step instead of wedging the workflow.
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
) -> bool {
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
            Ok(status) => status.success(),
            Err(e) => {
                tracing::warn!("condition eval failed for '{condition}': {e}");
                false
            }
        },
        _ = &mut timeout_fut => {
            tracing::warn!(
                "condition '{condition}' timed out after {CONDITION_TIMEOUT_SECS}s — skipping step"
            );
            false
        }
    }
    // On timeout the `cmd` future (still owned here) is dropped; `kill_on_drop`
    // reaps the spawned `sh` process so a `sleep infinity` condition cannot
    // outlive the bound.
}
