//! Generic external-tool runner for cross-tool benchmarking (WO 39.1 Phase 3).
//!
//! Spawns `claude -p`, `codex exec`, `opencode run`, or `kf-code run` as a
//! subprocess in a workspace dir, captures stdout/stderr/exit/wall-clock,
//! parses each tool's JSON usage shape, and returns an `ExternalToolReport`.
//!
//! ponytail: sync subprocess + per-tool JSON field picking. The upgrade path
//! is the LiteLLM gateway (Phase 4) — this runner already records `model`
//! and `gateway` is stored unimplemented at the CLI layer.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ExternalToolReport;

/// Which external coding agent to invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalTool {
    Claude,
    Codex,
    OpenCode,
    KfCode,
}

impl ExternalTool {
    pub fn as_str(self) -> &'static str {
        match self {
            ExternalTool::Claude => "claude",
            ExternalTool::Codex => "codex",
            ExternalTool::OpenCode => "opencode",
            ExternalTool::KfCode => "kf-code",
        }
    }

    /// The executable name used to spawn the tool.
    pub fn binary(self) -> &'static str {
        match self {
            ExternalTool::Claude => "claude",
            ExternalTool::Codex => "codex",
            ExternalTool::OpenCode => "opencode",
            // kf-code's installed binary name; the runner is invoked from
            // within the kf-code repo but the binary on PATH is `kf-code`.
            ExternalTool::KfCode => "kf-code",
        }
    }

    /// Parse a comma-separated `--tools` argument (`claude,codex,opencode`)
    /// into the enum variants. Unknown names are returned as errors so the
    /// CLI surfaces typos instead of silently skipping a tool.
    pub fn parse_csv(s: &str) -> Result<Vec<ExternalTool>> {
        let mut out = Vec::new();
        for part in s.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            out.push(Self::parse_one(p)?);
        }
        Ok(out)
    }

    /// Parse a single tool name. Unknown names error.
    pub fn parse_one(s: &str) -> Result<ExternalTool> {
        Ok(match s.trim() {
            "claude" => ExternalTool::Claude,
            "codex" => ExternalTool::Codex,
            "opencode" => ExternalTool::OpenCode,
            "kf-code" | "kfcode" => ExternalTool::KfCode,
            other => anyhow::bail!(
                "unknown tool `{other}` (expected one of: claude, codex, opencode, kf-code)"
            ),
        })
    }
}

/// Configuration for a single external-tool run.
#[derive(Debug, Clone)]
pub struct ExternalRunConfig {
    pub tool: ExternalTool,
    pub model: Option<String>,
    pub prompt: String,
    pub workspace_path: PathBuf,
    pub timeout_secs: u64,
    pub max_turns: Option<u32>,
    pub task_name: String,
}

/// Spawn the configured tool, wait up to `timeout_secs`, and parse the JSON
/// output for token usage. If the tool binary is not on PATH, returns a
/// report with `success=false` and an `stdout_excerpt` of "tool not found"
/// so the caller can continue with the remaining tools (ponytail: the spec
/// says missing tools should not abort the batch).
///
/// The child is spawned with piped stdout/stderr. We poll `try_wait` in a
/// 100ms loop so we can SIGKILL on deadline; on exit (or kill) we drain the
/// pipe handles to a String, then parse the tool's JSON usage shape.
pub fn run_external(cfg: &ExternalRunConfig) -> Result<ExternalToolReport> {
    if which_missing(cfg.tool.binary()) {
        return Ok(tool_not_found_report(cfg));
    }

    let mut cmd = build_command(cfg);
    cmd.current_dir(&cfg.workspace_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let deadline = start + Duration::from_secs(cfg.timeout_secs.max(1));

    let (exit_code, stdout) = loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = drain(child.stdout.take());
                let _stderr = drain(child.stderr.take());
                break (status.code(), stdout);
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let stdout = drain(child.stdout.take());
                    let wall = start.elapsed().as_secs_f64();
                    return Ok(ExternalToolReport {
                        tool_name: cfg.tool.as_str().to_string(),
                        task_name: cfg.task_name.clone(),
                        model: cfg.model.clone().unwrap_or_default(),
                        prompt: cfg.prompt.clone(),
                        success: false,
                        tokens_prompt: 0,
                        tokens_completion: 0,
                        tokens_total: 0,
                        cost_usd: None,
                        wall_clock_secs: wall,
                        exit_code: None,
                        stdout_excerpt: Some("timeout".to_string()),
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    let wall = start.elapsed().as_secs_f64();
    let success = exit_code == Some(0);

    let (tokens_prompt, tokens_completion, tokens_total, cost_usd) =
        parse_usage(cfg.tool, &stdout);

    Ok(ExternalToolReport {
        tool_name: cfg.tool.as_str().to_string(),
        task_name: cfg.task_name.clone(),
        model: cfg.model.clone().unwrap_or_default(),
        prompt: cfg.prompt.clone(),
        success,
        tokens_prompt,
        tokens_completion,
        tokens_total,
        cost_usd,
        wall_clock_secs: wall,
        exit_code,
        stdout_excerpt: Some(excerpt(&stdout)),
    })
}

/// Read a piped child handle to a String, returning "" on any error.
/// Generic over `ChildStdout` / `ChildStderr` so we can drain both pipes
/// with one helper.
fn drain<R: std::io::Read>(reader: Option<R>) -> String {
    let mut s = String::new();
    if let Some(mut r) = reader {
        let _ = r.read_to_string(&mut s);
    }
    s
}

/// Build the per-tool Command. The prompt is passed as a single arg so
/// shell-quoting is the OS's job, not ours.
fn build_command(cfg: &ExternalRunConfig) -> Command {
    let mut cmd = Command::new(cfg.tool.binary());
    match cfg.tool {
        ExternalTool::Claude => {
            // `claude -p "<prompt>" --output-format json --dangerously-skip-permissions`
            cmd.arg("-p")
                .arg(&cfg.prompt)
                .arg("--output-format")
                .arg("json")
                .arg("--dangerously-skip-permissions");
            if let Some(m) = &cfg.model {
                cmd.arg("--model").arg(m);
            }
        }
        ExternalTool::Codex => {
            // `codex exec "<prompt>" --json`
            cmd.arg("exec").arg(&cfg.prompt).arg("--json");
            if let Some(m) = &cfg.model {
                cmd.arg("--model").arg(m);
            }
        }
        ExternalTool::OpenCode => {
            // `opencode run "<prompt>" --format json`
            cmd.arg("run").arg(&cfg.prompt).arg("--format").arg("json");
            if let Some(m) = &cfg.model {
                cmd.arg("--model").arg(m);
            }
        }
        ExternalTool::KfCode => {
            // `kf-code run -p "<prompt>" --non-interactive --output json`
            cmd.arg("run")
                .arg("-p")
                .arg(&cfg.prompt)
                .arg("--non-interactive")
                .arg("--output")
                .arg("json");
            if let Some(m) = &cfg.model {
                cmd.arg("--model").arg(m);
            }
        }
    }
    if let Some(n) = cfg.max_turns {
        // Each tool has its own turns flag; pass the common long form and
        // let the tool ignore it if unsupported. ponytail: not all tools
        // honor --max-turns; the bench is still valid without it.
        cmd.arg("--max-turns").arg(n.to_string());
    }
    cmd
}

/// Check if a binary is missing from PATH. Uses `which` from std (no dep).
/// ponytail: shelling out to `command -v` avoids adding the `which` crate.
fn which_missing(bin: &str) -> bool {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    !matches!(status.map(|s| s.success()), Ok(true))
}

fn tool_not_found_report(cfg: &ExternalRunConfig) -> ExternalToolReport {
    ExternalToolReport {
        tool_name: cfg.tool.as_str().to_string(),
        task_name: cfg.task_name.clone(),
        model: cfg.model.clone().unwrap_or_default(),
        prompt: cfg.prompt.clone(),
        success: false,
        tokens_prompt: 0,
        tokens_completion: 0,
        tokens_total: 0,
        cost_usd: None,
        wall_clock_secs: 0.0,
        exit_code: Some(127),
        stdout_excerpt: Some("tool not found".to_string()),
    }
}

/// Take the first ~200 chars of stdout, single-line, for a quick excerpt.
/// Truncates on a UTF-8 char boundary so multi-byte content doesn't panic.
fn excerpt(s: &str) -> String {
    let one_line: String = s.lines().next().unwrap_or("").to_string();
    if one_line.chars().count() <= 200 {
        return one_line;
    }
    let cut = one_line
        .char_indices()
        .nth(200)
        .map(|(i, _)| i)
        .unwrap_or(one_line.len());
    format!("{}...", &one_line[..cut])
}

// ── Per-tool JSON parsing ──
//
// Each tool emits a different JSON shape for usage. We parse defensively:
// missing fields default to 0/None. The runner does not fail on a parse
// error — it just reports 0 tokens and keeps the stdout excerpt so the
// operator can see what the tool actually emitted.

/// Parse token usage (and cost if present) from the tool's JSON output.
/// Returns `(prompt_tokens, completion_tokens, total_tokens, cost_usd)`.
pub fn parse_usage(
    tool: ExternalTool,
    stdout: &str,
) -> (u64, u64, u64, Option<f64>) {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return (0, 0, 0, None),
    };
    match tool {
        ExternalTool::Claude => parse_claude_usage(&v),
        ExternalTool::Codex => parse_codex_usage(&v),
        ExternalTool::OpenCode => parse_opencode_usage(&v),
        ExternalTool::KfCode => parse_kfcode_usage(&v),
    }
}

// Claude Code `--output-format json` emits an envelope like:
// { "result": "...", "usage": { "input_tokens": 123, "output_tokens": 456 } }
// Cost is not reported by the CLI; we leave it None.
fn parse_claude_usage(v: &serde_json::Value) -> (u64, u64, u64, Option<f64>) {
    let usage = v.get("usage");
    let p = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let c = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let total = p + c;
    (p, c, total, None)
}

// Codex `exec --json` emits a final JSON line with a `usage` object:
// { "usage": { "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150 } }
// When the output is a stream of JSON lines, parse the LAST object that
// carries a `usage` field (the final summary).
fn parse_codex_usage(v: &serde_json::Value) -> (u64, u64, u64, Option<f64>) {
    // Codex may emit a top-level object with usage, or a stream we already
    // collapsed. The runner hands us whatever `stdout` parsed as a single
    // JSON value — if it's an array, take the last element with usage.
    let candidates: Vec<&serde_json::Value> = match v {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        _ => vec![v],
    };
    for cand in candidates.iter().rev() {
        if let Some(u) = cand.get("usage") {
            let p = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let c = u
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let t = u.get("total_tokens").and_then(|x| x.as_u64()).unwrap_or(p + c);
            return (p, c, t, None);
        }
    }
    (0, 0, 0, None)
}

// opencode `run --format json` emits a report with a `tokens` object:
// { "tokens": { "prompt": 100, "completion": 50, "total": 150 }, "cost": 0.01 }
fn parse_opencode_usage(v: &serde_json::Value) -> (u64, u64, u64, Option<f64>) {
    let t = v.get("tokens");
    let p = t
        .and_then(|x| x.get("prompt"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let c = t
        .and_then(|x| x.get("completion"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let total = t
        .and_then(|x| x.get("total"))
        .and_then(|x| x.as_u64())
        .unwrap_or(p + c);
    let cost = v.get("cost").and_then(|x| x.as_f64());
    (p, c, total, cost)
}

// kf-code `run --output json` emits the kf-code turn envelope, which has
// `cost_stats` with prompt/completion tokens and total cost. Shape:
// { "cost_stats": { "prompt_tokens": 100, "completion_tokens": 50,
//   "total_cost_usd": 0.012 } }
fn parse_kfcode_usage(v: &serde_json::Value) -> (u64, u64, u64, Option<f64>) {
    let cs = v.get("cost_stats");
    let p = cs
        .and_then(|x| x.get("prompt_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let c = cs
        .and_then(|x| x.get("completion_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let cost = cs
        .and_then(|x| x.get("total_cost_usd"))
        .and_then(|x| x.as_f64());
    (p, c, p + c, cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_handles_comma_list() {
        let tools = ExternalTool::parse_csv("claude,codex,opencode").unwrap();
        assert_eq!(
            tools,
            vec![
                ExternalTool::Claude,
                ExternalTool::Codex,
                ExternalTool::OpenCode
            ]
        );
    }

    #[test]
    fn parse_one_handles_single_name_and_whitespace() {
        assert_eq!(ExternalTool::parse_one("claude").unwrap(), ExternalTool::Claude);
        assert_eq!(ExternalTool::parse_one("  codex  ").unwrap(), ExternalTool::Codex);
        assert_eq!(ExternalTool::parse_one("kf-code").unwrap(), ExternalTool::KfCode);
        assert_eq!(ExternalTool::parse_one("kfcode").unwrap(), ExternalTool::KfCode);
    }

    #[test]
    fn parse_one_rejects_unknown() {
        let err = ExternalTool::parse_one("frobnicator").unwrap_err();
        assert!(err.to_string().contains("unknown tool `frobnicator`"));
    }

    #[test]
    fn parse_csv_handles_whitespace_and_kf_code_alias() {
        let tools = ExternalTool::parse_csv(" claude , kf-code , codex ").unwrap();
        assert_eq!(
            tools,
            vec![
                ExternalTool::Claude,
                ExternalTool::KfCode,
                ExternalTool::Codex
            ]
        );
    }

    #[test]
    fn parse_csv_rejects_unknown_tool() {
        let err = ExternalTool::parse_csv("claude,frobnicator").unwrap_err();
        assert!(
            err.to_string().contains("unknown tool `frobnicator`"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_csv_ignores_empty_segments() {
        let tools = ExternalTool::parse_csv("claude,,codex,").unwrap();
        assert_eq!(tools, vec![ExternalTool::Claude, ExternalTool::Codex]);
    }

    #[test]
    fn parse_claude_usage_extracts_input_output_tokens() {
        let json = r#"{
            "result": "done",
            "usage": { "input_tokens": 1234, "output_tokens": 567 }
        }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::Claude, json);
        assert_eq!(p, 1234);
        assert_eq!(c, 567);
        assert_eq!(t, 1801);
        assert!(cost.is_none());
    }

    #[test]
    fn parse_claude_usage_missing_usage_returns_zeros() {
        let json = r#"{ "result": "no usage here" }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::Claude, json);
        assert_eq!((p, c, t), (0, 0, 0));
        assert!(cost.is_none());
    }

    #[test]
    fn parse_claude_usage_invalid_json_returns_zeros() {
        let (p, c, t, cost) = parse_usage(ExternalTool::Claude, "not json at all");
        assert_eq!((p, c, t), (0, 0, 0));
        assert!(cost.is_none());
    }

    #[test]
    fn parse_codex_usage_extracts_token_fields() {
        let json = r#"{ "usage": { "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150 } }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::Codex, json);
        assert_eq!((p, c, t), (100, 50, 150));
        assert!(cost.is_none());
    }

    #[test]
    fn parse_codex_usage_takes_last_usage_in_array() {
        // Simulates a stream of JSON lines collapsed into an array.
        let json = r#"[
            { "type": "message", "text": "thinking..." },
            { "type": "message", "text": "more..." },
            { "usage": { "prompt_tokens": 200, "completion_tokens": 80, "total_tokens": 280 } }
        ]"#;
        let (p, c, t, _) = parse_usage(ExternalTool::Codex, json);
        assert_eq!((p, c, t), (200, 80, 280));
    }

    #[test]
    fn parse_codex_usage_no_usage_returns_zeros() {
        let json = r#"{ "type": "message", "text": "no usage" }"#;
        let (p, c, t, _) = parse_usage(ExternalTool::Codex, json);
        assert_eq!((p, c, t), (0, 0, 0));
    }

    #[test]
    fn parse_codex_usage_falls_back_to_sum_when_total_missing() {
        let json = r#"{ "usage": { "prompt_tokens": 70, "completion_tokens": 30 } }"#;
        let (p, c, t, _) = parse_usage(ExternalTool::Codex, json);
        assert_eq!((p, c, t), (70, 30, 100));
    }

    #[test]
    fn parse_opencode_usage_extracts_tokens_and_cost() {
        let json = r#"{ "tokens": { "prompt": 300, "completion": 120, "total": 420 }, "cost": 0.015 }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::OpenCode, json);
        assert_eq!((p, c, t), (300, 120, 420));
        assert_eq!(cost, Some(0.015));
    }

    #[test]
    fn parse_opencode_usage_missing_cost_returns_none() {
        let json = r#"{ "tokens": { "prompt": 10, "completion": 5, "total": 15 } }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::OpenCode, json);
        assert_eq!((p, c, t), (10, 5, 15));
        assert!(cost.is_none());
    }

    #[test]
    fn parse_opencode_usage_falls_back_to_sum_when_total_missing() {
        let json = r#"{ "tokens": { "prompt": 40, "completion": 12 } }"#;
        let (p, c, t, _) = parse_usage(ExternalTool::OpenCode, json);
        assert_eq!((p, c, t), (40, 12, 52));
    }

    #[test]
    fn parse_kfcode_usage_extracts_from_cost_stats() {
        let json = r#"{ "cost_stats": { "prompt_tokens": 500, "completion_tokens": 250, "total_cost_usd": 0.034 } }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::KfCode, json);
        assert_eq!((p, c, t), (500, 250, 750));
        assert_eq!(cost, Some(0.034));
    }

    #[test]
    fn parse_kfcode_usage_missing_cost_stats_returns_zeros() {
        let json = r#"{ "some_other_field": "x" }"#;
        let (p, c, t, cost) = parse_usage(ExternalTool::KfCode, json);
        assert_eq!((p, c, t), (0, 0, 0));
        assert!(cost.is_none());
    }

    #[test]
    fn excerpt_truncates_long_lines() {
        let long = "x".repeat(300);
        let ex = excerpt(&long);
        assert!(ex.ends_with("..."));
        assert_eq!(ex.len(), 203);
    }

    #[test]
    fn excerpt_keeps_short_lines_intact() {
        assert_eq!(excerpt("hello"), "hello");
    }

    #[test]
    fn excerpt_takes_first_line_only() {
        assert_eq!(excerpt("line1\nline2"), "line1");
    }

    #[test]
    fn tool_not_found_report_shape() {
        let cfg = ExternalRunConfig {
            tool: ExternalTool::Claude,
            model: Some("claude-3".into()),
            prompt: "p".into(),
            workspace_path: PathBuf::from("/tmp"),
            timeout_secs: 60,
            max_turns: None,
            task_name: "t".into(),
        };
        let r = tool_not_found_report(&cfg);
        assert!(!r.success);
        assert_eq!(r.exit_code, Some(127));
        assert_eq!(r.stdout_excerpt.as_deref(), Some("tool not found"));
        assert_eq!(r.tokens_total, 0);
    }

    #[test]
    fn build_command_claude_uses_dash_p_and_json_format() {
        let cfg = ExternalRunConfig {
            tool: ExternalTool::Claude,
            model: Some("claude-3".into()),
            prompt: "do the thing".into(),
            workspace_path: PathBuf::from("/tmp"),
            timeout_secs: 60,
            max_turns: Some(5),
            task_name: "t".into(),
        };
        let cmd = build_command(&cfg);
        let prog = cmd.get_program();
        assert_eq!(prog, std::ffi::OsStr::new("claude"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "do the thing");
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"json".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"claude-3".to_string()));
        assert!(args.contains(&"--max-turns".to_string()));
        assert!(args.contains(&"5".to_string()));
    }

    #[test]
    fn build_command_codex_uses_exec_subcommand() {
        let cfg = ExternalRunConfig {
            tool: ExternalTool::Codex,
            model: None,
            prompt: "fix it".into(),
            workspace_path: PathBuf::from("/tmp"),
            timeout_secs: 60,
            max_turns: None,
            task_name: "t".into(),
        };
        let cmd = build_command(&cfg);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "fix it");
        assert!(args.contains(&"--json".to_string()));
        // No model → no --model flag.
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn build_command_opencode_uses_run_subcommand() {
        let cfg = ExternalRunConfig {
            tool: ExternalTool::OpenCode,
            model: None,
            prompt: "refactor".into(),
            workspace_path: PathBuf::from("/tmp"),
            timeout_secs: 60,
            max_turns: None,
            task_name: "t".into(),
        };
        let cmd = build_command(&cfg);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "refactor");
        assert!(args.contains(&"--format".to_string()));
        assert!(args.contains(&"json".to_string()));
    }

    #[test]
    fn build_command_kfcode_uses_run_subcommand_with_non_interactive() {
        let cfg = ExternalRunConfig {
            tool: ExternalTool::KfCode,
            model: None,
            prompt: "ship it".into(),
            workspace_path: PathBuf::from("/tmp"),
            timeout_secs: 60,
            max_turns: None,
            task_name: "t".into(),
        };
        let cmd = build_command(&cfg);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "run");
        assert!(args.contains(&"--non-interactive".to_string()));
        assert!(args.contains(&"--output".to_string()));
        assert!(args.contains(&"json".to_string()));
    }

    #[test]
    fn external_tool_as_str_matches_binary_except_kfcode() {
        // kf-code's display name uses a hyphen; binary is also `kf-code`.
        assert_eq!(ExternalTool::Claude.as_str(), "claude");
        assert_eq!(ExternalTool::Codex.as_str(), "codex");
        assert_eq!(ExternalTool::OpenCode.as_str(), "opencode");
        assert_eq!(ExternalTool::KfCode.as_str(), "kf-code");
        assert_eq!(ExternalTool::KfCode.binary(), "kf-code");
    }
}