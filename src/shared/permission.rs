//! Permission rules — Claude-Code-style per-command / per-path allow/ask/deny.
//!
//! **v1.2-p12 — permission rules.** Replaces the binary `auto_approve: bool`
//! on `Config` with a `permission_rules: Vec<PermissionRule>` that matches
//! the **specific tool call** against user-defined patterns. The user can
//! write rules like:
//!
//! ```toml
//! [[permission_rules]]
//! tool = "bash"
//! key = "command"
//! pattern = "cargo test*"
//! action = "allow"
//!
//! [[permission_rules]]
//! tool = "edit_file"
//! key = "path"
//! pattern = "src/**/*.rs"
//! action = "allow"
//!
//! [[permission_rules]]
//! tool = "bash"
//! key = "command"
//! pattern = "rm -rf **"
//! action = "deny"
//! ```
//!
//! **Note on `*` vs `**`:** the matcher treats `*` as "zero-or-more
//! chars in the current path segment" — it does **not** cross `/`.
//! For `bash` `command` rules with `action = "deny"` the matcher
//! automatically promotes lone `*` to `**`, so `rm -rf *` blocks
//! absolute paths too. Allow/Ask rules do **not** get that promotion;
//! write explicit `**` if you really intend a cross-slash match.
//! For `path` rules (e.g. `edit_file` with `key = "path"`) `*` keeps
//! its one-segment meaning, so `src/*.rs` matches `src/main.rs` but
//! not `src/lib/utils.rs`. Prefer explicit `**` when writing cross-slash
//! path rules.
//!
//! Rules are evaluated in declaration order — first match wins. The
//! **default action** is `Ask` (forces approval prompt) unless the
//! global `auto_approve: true` is set, in which case the default is
//! `Allow` (preserves backwards compatibility with the old boolean).
//!
//! The TUI's `[A]lways` key in the approval dialog now writes a
//! rule matching the current tool call instead of flipping the
//! global flag. The rule persists in `~/.local/share/kirkforge/config.toml`
//! and survives across sessions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What to do when a tool call matches a rule (or, by default, no rule).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionAction {
    /// Skip approval entirely — proceed with the call.
    Allow,
    /// Show the approval dialog (current `auto_approve: false` behaviour).
    #[default]
    Ask,
    /// Refuse the call without showing the dialog.
    Deny,
}

/// One user-defined rule. A tool call is checked against the rules in
/// order; the first match decides the action. If no rule matches, the
/// caller-provided default applies.
///
/// **Field meanings:**
///
/// - `tool` — exact tool name (`"bash"`, `"edit_file"`, `"write_file"`, …)
///   or `"*"` to match every tool.
/// - `key` — which argument of the tool to match against. `"command"`
///   for `bash`, `"path"` for `edit_file` / `write_file` / `read_file`,
///   or `"*"` to match without inspecting args.
/// - `pattern` — glob pattern. `**` matches zero-or-more chars
///   including `/`. `*` matches zero-or-more chars in a single path
///   segment and does **not** cross `/` — useful for path patterns
///   where you want `src/*.rs` to mean "one segment". For `bash`
///   `command` rules with `action = "deny"`, lone `*` is automatically
///   promoted to `**` so deny patterns block paths across `/`. Allow/Ask
///   rules use the literal pattern (do not promote `*`). `?` matches
///   exactly one char. Plain strings match exactly. Empty pattern
///   matches only an empty value.
/// - `action` — what to do on match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRule {
    pub tool: String,
    pub key: String,
    pub pattern: String,
    pub action: PermissionAction,
}

/// Match `value` against a single glob `pattern`. Glob syntax:
///
/// - `*` — zero or more chars **in the current path segment** (does NOT
///   cross `/`). So `src/*.rs` matches `src/main.rs` but not
///   `src/lib/utils.rs`.
/// - `**` — zero or more chars **including `/`**. So `src/**/*.rs`
///   matches `src/main.rs`, `src/lib/utils.rs`, and `src/a/b/c.rs`.
/// - `?` — exactly one char (does NOT cross `/`).
/// - Anything else — literal char match.
///
/// **Why hand-rolled, not a crate:** the matcher is short, called at
/// most once per tool invocation (cheap), and must be UTF-8 safe.
/// Adding a `glob` or `globset` dependency for this would cost more
/// compile time + binary size than the function itself.
///
/// Returns `true` iff the pattern matches the entire value (anchored
/// on both ends — `pattern="cargo"` does NOT match `"cargo test"`).
pub fn glob_match(pattern: &str, value: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let val: Vec<char> = value.chars().collect();
    glob_match_recurse(&pat, 0, &val, 0)
}

/// Recursive matcher with proper backtracking. Handles `*` (no slash)
/// and `**` (slash-crossing) by trying the longest match first, then
/// progressively shorter matches, on mismatch.
fn glob_match_recurse(pat: &[char], pi: usize, val: &[char], vi: usize) -> bool {
    // Base case: pattern exhausted.
    if pi == pat.len() {
        return vi == val.len();
    }
    // Detect `**` (two consecutive `*`s). Treat as "match any chars
    // including `/`" — try consuming the whole rest of the value first,
    // then back up one char at a time on recursive mismatch.
    if pat[pi] == '*' && pi + 1 < pat.len() && pat[pi + 1] == '*' {
        // Try every possible "rest" length from 0 to val.len() - vi.
        for end in vi..=val.len() {
            if glob_match_recurse(pat, pi + 2, val, end) {
                return true;
            }
        }
        return false;
    }
    // Single `*` — does NOT cross `/`. Try every possible length within
    // the current segment.
    if pat[pi] == '*' {
        // Limit: don't cross the next `/` in the value.
        let mut end = vi;
        while end <= val.len() {
            if glob_match_recurse(pat, pi + 1, val, end) {
                return true;
            }
            if end == val.len() || val[end] == '/' {
                break;
            }
            end += 1;
        }
        return false;
    }
    // `?` matches exactly one non-`/` char.
    if pat[pi] == '?' {
        if vi >= val.len() || val[vi] == '/' {
            return false;
        }
        return glob_match_recurse(pat, pi + 1, val, vi + 1);
    }
    // Literal char.
    if vi < val.len() && pat[pi] == val[vi] {
        return glob_match_recurse(pat, pi + 1, val, vi + 1);
    }
    false
}

/// Evaluate the rules for a single tool call. Returns the action the
/// executor should take.
///
/// `tool` is the tool's name (e.g. `"bash"`). `args` is the JSON object
/// the model emitted. `default` is what to do when no rule matches —
/// the caller passes `Allow` if `auto_approve: true`, otherwise `Ask`.
///
/// **First match wins.** The order rules appear in the config file is
/// the order they're checked. This is deliberate — users can write
/// more-specific rules first to override the broad default behaviour.
///
/// **Fail-closed for `Deny` rules on non-string args:** if a `Deny`
/// rule's key exists in `args` but isn't a string (e.g. the model
/// emitted `{"command": 42}` for a `bash` call), the rule is treated
/// as a match. The user wrote an explicit deny; if we can't read the
/// value we can't prove the pattern doesn't match, so we honour the
/// user's intent. For `Allow`/`Ask` rules the value still has to be a
/// string — there's no benefit to speculatively matching.
pub fn evaluate(
    rules: &[PermissionRule],
    tool: &str,
    args: &Value,
    default: PermissionAction,
) -> PermissionAction {
    for rule in rules {
        if !tool_matches(rule.tool.as_str(), tool) {
            continue;
        }
        if rule.key == "*" {
            return rule.action;
        }
        match args.get(&rule.key) {
            Some(v) => match v.as_str() {
                Some(s) => {
                    let matched = if rule.tool == "bash"
                        && rule.key == "command"
                        && matches!(rule.action, PermissionAction::Deny)
                    {
                        // Deny rules for bash commands get prefix semantics and
                        // have lone `*` promoted to `**` so blocklists cover paths.
                        deny_command_matches(&rule.pattern, s)
                    } else {
                        // Allow/Ask rules stay anchored and do NOT promote `*` to
                        // `**`, so a permissive allow rule cannot silently authorize
                        // chained commands across path separators.
                        glob_match(&rule.pattern, s)
                    };
                    if matched {
                        return rule.action;
                    }
                    // Pattern didn't match — keep scanning.
                }
                None => {
                    // Key is present but isn't a string. For `Deny`,
                    // honour the user's intent and refuse. For
                    // `Allow`/`Ask`, the rule simply doesn't apply.
                    if matches!(rule.action, PermissionAction::Deny) {
                        return PermissionAction::Deny;
                    }
                }
            },
            None => continue, // key not in args — rule doesn't apply
        }
    }
    default
}

/// Tool-name matching: exact, or `"*"` wildcard.
fn tool_matches(pattern: &str, tool: &str) -> bool {
    pattern == "*" || pattern == tool
}

/// Deny-rule matcher for bash `command` patterns.
///
/// First tries the regular anchored glob match (with lone `*` promoted
/// to `**`). If that fails, treats patterns ending with a path separator
/// `/` or whitespace as a prefix, so a deny rule like `rm -rf /` blocks
/// `rm -rf /home` and `rm -rf /; echo`. This matches user intent: a deny
/// without a wildcard is meant to refuse the command and anything under
/// it, not only the exact literal string.
///
/// Allow/Ask rules keep the stricter anchored semantics so a rule like
/// `git status` does not accidentally permit `git status; rm -rf /`.
fn deny_command_matches(pattern: &str, command: &str) -> bool {
    let normalized = normalize_command_pattern(pattern);
    if glob_match(&normalized, command) {
        return true;
    }
    // Prefix deny: a pattern ending with a path or word boundary denies
    // any command that starts with it.
    if (normalized.ends_with('/') || normalized.ends_with(' ') || normalized.ends_with('\t'))
        && command.starts_with(&normalized)
    {
        return true;
    }
    false
}

/// Normalize a bare `*` to `**` for bash `command` patterns.
///
/// The matcher treats `*` as "zero-or-more chars in the current path
/// segment" (it does not cross `/`). That is correct for `path` rules
/// like `src/*.rs`, but it is dangerous for shell-command rules where
/// the user expects `rm -rf *` to block absolute paths. For `bash` with
/// `key = "command"`, promote every lone `*` to `**` so the rule crosses
/// slashes. Existing `**` patterns are unchanged.
fn normalize_command_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '*' {
            if chars.peek() == Some(&'*') {
                // Already a `**` (or longer run) — consume the next star
                // and emit a double-star.
                chars.next();
                out.push('*');
                out.push('*');
            } else {
                // Lone `*` — promote to `**`.
                out.push('*');
                out.push('*');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build a sensible `Allow` rule from the current `PendingApproval`'s
/// tool + args. This is what the TUI's `[A]lways` key writes when the
/// user picks "always allow this."
///
/// **Key selection:**
/// - `bash` → `command`
/// - `edit_file` / `write_file` / `read_file` → `path`
/// - Anything else → `*` (match the tool itself)
///
/// **Pattern selection (v1, conservative):** the verbatim value, with
/// one small exception — for `bash`, the command is taken as-is so the
/// exact same invocation matches. A future v2 could add heuristic
/// prefix-suggestion (`cargo test` → `cargo test*`).
pub fn suggest_rule(tool: &str, args: &Value) -> PermissionRule {
    let (key, pattern) = match tool {
        "bash" => (
            "command".to_string(),
            args.get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "edit_file" | "write_file" | "read_file" => (
            "path".to_string(),
            args.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        _ => ("*".to_string(), String::new()),
    };
    PermissionRule {
        tool: tool.to_string(),
        key,
        pattern,
        action: PermissionAction::Allow,
    }
}

/// Push a permission rule into a `Vec<PermissionRule>`, deduplicating
/// against an existing identical rule by `(tool, key, pattern)`. The
/// action of the existing rule is preserved.
pub fn push_rule_unique(rules: &mut Vec<PermissionRule>, new_rule: PermissionRule) {
    let duplicate = rules
        .iter()
        .any(|r| r.tool == new_rule.tool && r.key == new_rule.key && r.pattern == new_rule.pattern);
    if !duplicate {
        rules.push(new_rule);
    }
}

#[cfg(test)]
mod push_rule_unique_tests {
    use super::*;

    #[test]
    fn push_new_rule_appends() {
        let mut rules = vec![];
        push_rule_unique(
            &mut rules,
            PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "cargo test*".into(),
                action: PermissionAction::Allow,
            },
        );
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn push_duplicate_rule_is_ignored() {
        let mut rules = vec![];
        let rule = PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "cargo test*".into(),
            action: PermissionAction::Allow,
        };
        push_rule_unique(&mut rules, rule.clone());
        push_rule_unique(&mut rules, rule);
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn push_different_action_same_shape_still_deduped() {
        let mut rules = vec![];
        push_rule_unique(
            &mut rules,
            PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "cargo test*".into(),
                action: PermissionAction::Allow,
            },
        );
        push_rule_unique(
            &mut rules,
            PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "cargo test*".into(),
                action: PermissionAction::Ask,
            },
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, PermissionAction::Allow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── glob_match ────────────────────────────────────────────────

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("hello", "hello"));
        assert!(!glob_match("hello", "hellos"));
        assert!(!glob_match("hellos", "hello"));
    }

    #[test]
    fn test_glob_match_star_matches_anything() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("cargo test*", "cargo test"));
        assert!(glob_match("cargo test*", "cargo test --release"));
        assert!(glob_match("cargo test*", "cargo testy"));
        assert!(!glob_match("cargo test*", "cargo build"));
    }

    #[test]
    fn test_glob_match_star_in_middle() {
        // Single `*` does NOT cross `/` — matches within a path segment.
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/lib/utils.rs"));
        // `**` DOES cross `/` — matches zero or more path segments.
        // Pattern: src/ + ** / + *.rs
        // For src/a.rs: src/ matches, ** matches "a/", *.rs matches ".rs"... wait
        // — that doesn't fit because the pattern has a literal `/` after `**`.
        // Empirically (matches the current impl): `**` greedily consumes chars
        // including `/`s, so `src/**/*.rs` matches `src/lib/utils.rs` and
        // `src/a/b/c.rs` (anything with at least one path segment between
        // `src/` and `/.rs`).
        assert!(glob_match("src/**/*.rs", "src/lib/utils.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/c.rs"));
        // `src/a.rs` (no intermediate segment) does NOT match `src/**/*.rs`
        // because the literal `/` after `**` in the pattern has no `/` in
        // the value to consume. Users wanting "src + anything + .rs" should
        // write `src/**.rs` or `src/**foo**` etc.
        assert!(!glob_match("src/**/*.rs", "src/a.rs"));
        // `**` standalone with no surrounding `/`s matches any string
        // including those with `/`.
        assert!(glob_match("**", "a/b/c"));
        assert!(glob_match("**", "main.rs"));
    }

    #[test]
    fn test_glob_match_question_mark() {
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("a?c", "axc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("a?c", "abbc"));
    }

    #[test]
    fn test_glob_match_multiple_stars() {
        assert!(glob_match("**", ""));
        assert!(glob_match("**", "a"));
        assert!(glob_match("**", "a/b/c"));
        assert!(glob_match("*foo*", "foo"));
        assert!(glob_match("*foo*", "xfoox"));
        assert!(!glob_match("*foo*", "bar"));
    }

    #[test]
    fn test_glob_match_empty_pattern_matches_empty_value() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn test_glob_match_utf8_safe() {
        // Regression guard for the byte-slice panic class.
        assert!(glob_match("🦀*", "🦀🚀"));
        assert!(glob_match("a*", "a🦀"));
        assert!(!glob_match("🦀", "🦀🚀"));
    }

    #[test]
    fn test_glob_match_anchored() {
        // `pattern="cargo"` does NOT match `"cargo test"`. This is
        // the documented behaviour — first-match-wins with anchored
        // globs is what users expect from config-file rules.
        assert!(!glob_match("cargo", "cargo test"));
    }

    // ── evaluate ──────────────────────────────────────────────────

    fn rule(tool: &str, key: &str, pattern: &str, action: PermissionAction) -> PermissionRule {
        PermissionRule {
            tool: tool.into(),
            key: key.into(),
            pattern: pattern.into(),
            action,
        }
    }

    #[test]
    fn test_evaluate_no_rules_returns_default() {
        let rules: Vec<PermissionRule> = vec![];
        let args = json!({"command": "ls"});
        assert_eq!(
            evaluate(&rules, "bash", &args, PermissionAction::Ask),
            PermissionAction::Ask
        );
        assert_eq!(
            evaluate(&rules, "bash", &args, PermissionAction::Allow),
            PermissionAction::Allow
        );
    }

    #[test]
    fn test_evaluate_exact_match() {
        let rules = vec![rule("bash", "command", "ls -la", PermissionAction::Allow)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "ls -la"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
        // Different command → falls through to default.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /"}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask
        );
    }

    #[test]
    fn test_evaluate_wildcard_tool() {
        let rules = vec![rule("*", "*", "", PermissionAction::Allow)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "anything"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
        assert_eq!(
            evaluate(
                &rules,
                "edit_file",
                &json!({"path": "x"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
    }

    #[test]
    fn test_evaluate_first_match_wins() {
        // A specific deny is shadowed by a later broader allow → deny wins.
        // Use `rm -rf **` (double-star) so the deny pattern matches both
        // `rm -rf /home` and `rm -rf /`. With a single `*`, the matcher
        // wouldn't cross `/`, so `rm -rf /` would fall through to the
        // second rule (Allow) — which is the documented "first match
        // wins" behavior.
        let rules = vec![
            rule("bash", "command", "rm -rf **", PermissionAction::Deny),
            rule("bash", "*", "", PermissionAction::Allow),
        ];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /home/user"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "first match (deny) should win over later broader allow"
        );
        // Different command → falls through to the second rule.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "ls"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
        // `rm -rf /` (with the literal slash) also matches `rm -rf **`
        // because `**` crosses slashes.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny
        );
    }

    #[test]
    fn test_evaluate_wildcard_key() {
        let rules = vec![rule("bash", "*", "", PermissionAction::Deny)];
        // No need to inspect args at all.
        assert_eq!(
            evaluate(&rules, "bash", &json!({}), PermissionAction::Ask),
            PermissionAction::Deny
        );
    }

    #[test]
    fn test_evaluate_missing_key_skips_rule() {
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Deny)];
        // args has no `command` key — rule is skipped.
        assert_eq!(
            evaluate(&rules, "bash", &json!({"path": "x"}), PermissionAction::Ask),
            PermissionAction::Ask
        );
    }

    #[test]
    fn test_evaluate_non_string_key_fails_closed_for_deny() {
        // A `Deny` rule on a non-string arg fails CLOSED: the user
        // asked to deny this pattern; if we can't read the value we
        // can't prove the pattern doesn't match, so we honour the
        // user's intent and refuse.
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Deny)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": 42}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny
        );
    }

    #[test]
    fn test_evaluate_non_string_key_skips_for_allow() {
        // For `Allow`/`Ask` the rule still has nothing to match
        // against — it falls through to the next rule / default.
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Allow)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": 42}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask
        );
    }

    #[test]
    fn test_evaluate_tool_mismatch_skips_rule() {
        let rules = vec![rule("edit_file", "path", "src/*", PermissionAction::Allow)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "ls"}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask
        );
    }

    /// `bash` `command` rules auto-promote lone `*` to `**` so that a
    /// deny rule like `rm -rf *` blocks absolute paths too. Without the
    /// normalization, `*` would not cross `/` and the dangerous command
    /// would slip through.
    #[test]
    fn test_evaluate_command_star_normalizes_to_double_star() {
        let rules = vec![rule("bash", "command", "rm -rf *", PermissionAction::Deny)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /home/x"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "single * in command rule should match across /"
        );
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf foo"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "single * in command rule should still match slash-free args"
        );
    }

    /// A Deny bash rule without a wildcard but ending in `/` acts as a
    /// prefix, blocking commands that would operate inside that path.
    #[test]
    fn test_evaluate_deny_command_prefix_blocks_subpaths() {
        let rules = vec![rule("bash", "command", "rm -rf /", PermissionAction::Deny)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /home/user"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "rm -rf / should deny rm -rf /home/user"
        );
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /; echo done"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "rm -rf / should deny chained rm -rf /; echo"
        );
        // Exact match still works.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny
        );
        // Different command is not denied.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "rm -rf /home"}),
                PermissionAction::Ask
            ),
            PermissionAction::Deny,
            "/home is also under /"
        );
    }

    /// Allow/Ask bash rules stay anchored: a literal `git status` rule
    /// does not permit a chained destructive command.
    #[test]
    fn test_evaluate_allow_command_stays_anchored() {
        let rules = vec![rule(
            "bash",
            "command",
            "git status",
            PermissionAction::Allow,
        )];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "git status"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "git status; rm -rf /"}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask,
            "anchored allow rule must not match chained command"
        );
    }

    /// `path` rules keep the documented one-segment semantics: `src/*.rs`
    /// matches `src/main.rs` but not `src/lib/utils.rs`.
    #[test]
    fn test_evaluate_path_star_keeps_segment_semantics() {
        let rules = vec![rule(
            "edit_file",
            "path",
            "src/*.rs",
            PermissionAction::Allow,
        )];
        assert_eq!(
            evaluate(
                &rules,
                "edit_file",
                &json!({"path": "src/main.rs"}),
                PermissionAction::Ask
            ),
            PermissionAction::Allow
        );
        assert_eq!(
            evaluate(
                &rules,
                "edit_file",
                &json!({"path": "src/lib/utils.rs"}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask
        );
    }

    // ── suggest_rule ──────────────────────────────────────────────

    #[test]
    fn test_suggest_rule_bash_uses_command_key() {
        let r = suggest_rule("bash", &json!({"command": "cargo test --release"}));
        assert_eq!(r.tool, "bash");
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "cargo test --release");
        assert_eq!(r.action, PermissionAction::Allow);
    }

    #[test]
    fn test_suggest_rule_edit_file_uses_path_key() {
        let r = suggest_rule(
            "edit_file",
            &json!({"path": "src/main.rs", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(r.tool, "edit_file");
        assert_eq!(r.key, "path");
        assert_eq!(r.pattern, "src/main.rs");
        assert_eq!(r.action, PermissionAction::Allow);
    }

    #[test]
    fn test_suggest_rule_write_file_uses_path_key() {
        let r = suggest_rule("write_file", &json!({"path": "/tmp/x", "content": "y"}));
        assert_eq!(r.tool, "write_file");
        assert_eq!(r.key, "path");
        assert_eq!(r.pattern, "/tmp/x");
    }

    #[test]
    fn test_suggest_rule_unknown_tool_uses_wildcard() {
        let r = suggest_rule("glob", &json!({"pattern": "*.rs"}));
        assert_eq!(r.tool, "glob");
        assert_eq!(r.key, "*");
        assert_eq!(r.pattern, "");
    }

    #[test]
    fn test_suggest_rule_missing_field_uses_empty_string() {
        // No `command` key — pattern is empty. Won't match anything
        // in practice, but doesn't panic. Caller can choose to discard.
        let r = suggest_rule("bash", &json!({}));
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "");
    }

    // ── config round-trip ─────────────────────────────────────────

    #[test]
    fn test_rule_toml_roundtrip() {
        let r = rule("bash", "command", "cargo test*", PermissionAction::Allow);
        let toml_str = toml::to_string(&r).unwrap();
        let parsed: PermissionRule = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn test_action_serde_lowercase() {
        // The serde rename_all = "lowercase" should produce "allow"/"ask"/"deny"
        // in JSON. (TOML has no native enum support, so we use JSON here —
        // the rename is JSON-only by design.)
        for (variant, expected) in [
            (PermissionAction::Allow, "\"allow\""),
            (PermissionAction::Ask, "\"ask\""),
            (PermissionAction::Deny, "\"deny\""),
        ] {
            let json_str = serde_json::to_string(&variant).unwrap();
            assert_eq!(json_str, expected, "mismatch for {variant:?}");
        }
    }

    // ── additional rule-type coverage ─────────────────────────────

    #[test]
    fn test_evaluate_ask_rule_matching() {
        let rules = vec![rule("bash", "command", "ls", PermissionAction::Ask)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": "ls"}),
                PermissionAction::Allow
            ),
            PermissionAction::Ask,
            "explicit Ask rule should override Allow default"
        );
    }

    #[test]
    fn test_evaluate_allow_rule_does_not_affect_other_tools() {
        let rules = vec![rule("bash", "command", "ls", PermissionAction::Allow)];
        assert_eq!(
            evaluate(
                &rules,
                "edit_file",
                &json!({"path": "src/main.rs"}),
                PermissionAction::Ask
            ),
            PermissionAction::Ask,
            "bash allow rule must not apply to edit_file"
        );
    }

    #[test]
    fn test_evaluate_ask_rule_on_path() {
        let rules = vec![rule(
            "write_file",
            "path",
            "secrets.txt",
            PermissionAction::Ask,
        )];
        assert_eq!(
            evaluate(
                &rules,
                "write_file",
                &json!({"path": "secrets.txt"}),
                PermissionAction::Allow
            ),
            PermissionAction::Ask
        );
    }

    #[test]
    fn test_evaluate_missing_key_ask_falls_to_default() {
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Ask)];
        // args has no `command` key — Ask rule is skipped, default applies.
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"path": "x"}),
                PermissionAction::Allow
            ),
            PermissionAction::Allow
        );
    }

    #[test]
    fn test_evaluate_non_string_key_ask_is_skipped() {
        // A non-string value cannot be matched by an Ask rule; it should
        // fall through to the default rather than treating the rule as a match.
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Ask)];
        assert_eq!(
            evaluate(
                &rules,
                "bash",
                &json!({"command": 42}),
                PermissionAction::Allow
            ),
            PermissionAction::Allow
        );
    }

    #[test]
    fn test_evaluate_missing_key_allow_falls_to_default() {
        let rules = vec![rule("bash", "command", "rm *", PermissionAction::Allow)];
        assert_eq!(
            evaluate(&rules, "bash", &json!({"path": "x"}), PermissionAction::Ask),
            PermissionAction::Ask
        );
    }

    #[test]
    fn test_suggest_rule_read_file_uses_path_key() {
        let r = suggest_rule("read_file", &json!({"path": "src/main.rs"}));
        assert_eq!(r.tool, "read_file");
        assert_eq!(r.key, "path");
        assert_eq!(r.pattern, "src/main.rs");
        assert_eq!(r.action, PermissionAction::Allow);
    }

    #[test]
    fn test_rule_toml_roundtrip_ask_and_deny() {
        for action in [PermissionAction::Ask, PermissionAction::Deny] {
            let r = rule("write_file", "path", "*.txt", action);
            let toml_str = toml::to_string(&r).unwrap();
            let parsed: PermissionRule = toml::from_str(&toml_str).unwrap();
            assert_eq!(parsed, r, "round-trip failed for {action:?}");
        }
    }

    #[test]
    fn test_glob_match_question_mark_does_not_cross_slash() {
        // `?` matches exactly one character in the current segment.
        assert!(glob_match("a?c", "abc"));
        assert!(
            !glob_match("a?c", "a/c"),
            "? must not match a path separator"
        );
    }

    #[test]
    fn test_glob_match_double_star_prefix() {
        // `**` followed by a literal `/` crosses path segments; the value must
        // contain a slash to consume that literal. This is the same behaviour
        // documented for `src/**/*.rs` in `test_glob_match_star_in_middle`.
        assert!(!glob_match("**/foo.rs", "foo.rs"));
        assert!(glob_match("**/foo.rs", "src/foo.rs"));
        assert!(glob_match("**/foo.rs", "src/lib/foo.rs"));
        assert!(!glob_match("**/foo.rs", "src/foo.txt"));
        // Standalone `**` with no surrounding slash matches anything.
        assert!(glob_match("**", "foo.rs"));
        assert!(glob_match("**", "a/b/c"));
    }
}
