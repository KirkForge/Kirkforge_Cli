//! Non-interactive / line-mode event emission and CLI path resolution.
//! Extracted from the binary root so `mod.rs` stays focused on argument
//! parsing, session setup, and the multi-turn driver loop.

use kirkforge::session;
use std::io::Write;

/// Serialize a JSON value and emit it as one stream-json line.
///
/// `serde_json::to_string` can fail only for non-finite floats; if that
/// somehow happens (e.g. a corrupted cost value), we log a warning and
/// skip the line rather than panicking in the headless output path.
fn print_json_line(value: &serde_json::Value) {
    match serde_json::to_string(value) {
        Ok(line) => println!("{line}"),
        Err(e) => tracing::warn!("failed to serialize stream-json event: {}", e),
    }
}

/// Per-turn event emission, extracted from the pre-M4 single-turn
/// loop so the multi-turn driver can call it once per turn without
/// duplicating the 165-line match. Mutates the running totals in
/// place; the caller reads `final_error` directly for the JSON summary.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_turn_events(
    events: &[session::executor::TurnEvent],
    output: kirkforge::shared::OutputFormat,
    total_prompt_tokens: &mut usize,
    total_completion_tokens: &mut usize,
    cumulative_cost: &mut f64,
    tool_records: &mut Vec<kirkforge::shared::ToolCallRecord>,
    final_error: &mut Option<String>,
) {
    // Per-tool timing + structured records for the JSON summary.
    // `ToolStart` arms the timer; the matching `ToolResult` reads
    // it and pushes a `ToolCallRecord` into `tool_records`. Tools
    // are dispatched sequentially by the executor, so a single
    // `Option` for the in-flight call is sufficient — we don't
    // need to key by id. The previous implementation emitted
    // `tool_calls: vec![]` regardless of reality (GPT 5.5 #13);
    // this fixes it.
    let mut in_flight: Option<(String, serde_json::Value, std::time::Instant)> = None;

    for event in events {
        match event {
            session::executor::TurnEvent::Token(t) => {
                if output == kirkforge::shared::OutputFormat::Text {
                    print!("{t}");
                    if let Err(e) = std::io::stdout().flush() {
                        tracing::debug!(error = %e, "failed to flush stdout token");
                    }
                } else if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "token", "content": t});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::Thinking(t) => {
                if output == kirkforge::shared::OutputFormat::Text {
                    eprintln!("\n[thinking] {t}");
                } else if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "thinking", "content": t});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::ToolStart { name, args } => {
                if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "tool_start", "name": name});
                    print_json_line(&line);
                }
                // Arm the in-flight timer for the matching ToolResult.
                // If we somehow see a second ToolStart without an
                // intervening ToolResult (shouldn't happen given the
                // executor's dispatch order, but defensive), the older
                // record is dropped — better than accumulating stale
                // timers.
                in_flight = Some((name.clone(), args.clone(), std::time::Instant::now()));
            }
            session::executor::TurnEvent::ToolResult {
                name,
                output: result,
                success,
            } => {
                if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "tool_result",
                        "name": name,
                        "content": result,
                    });
                    print_json_line(&line);
                } else if output == kirkforge::shared::OutputFormat::Text {
                    // Keep non-interactive output compact: one line per tool,
                    // and only the body if it failed. Successful tool churn is
                    // the main source of terminal spam.
                    let status = if *success { "ok" } else { "FAIL" };
                    eprintln!("[tool {name} -> {status}]");
                    if !success {
                        eprintln!("{result}");
                    }
                }
                // If we have a matching in-flight record, fold it
                // into a ToolCallRecord and push. Name mismatch
                // (shouldn't happen but be defensive) falls back to
                // empty args + zero duration.
                if let Some((start_name, start_args, start_time)) = in_flight.take() {
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    let record = kirkforge::shared::ToolCallRecord {
                        name: start_name,
                        arguments: start_args,
                        result: result.clone(),
                        success: *success,
                        duration_ms,
                    };
                    tool_records.push(record);
                    // If the name in the result doesn't match the
                    // start (paranoia), prefer the start name. We
                    // already used `start_name`; nothing to do.
                }
            }
            session::executor::TurnEvent::Verification { message, success } => {
                if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "verification",
                        "message": message,
                        "success": success,
                    });
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::Error(e) => {
                *final_error = Some(e.clone());
                if output == kirkforge::shared::OutputFormat::Text {
                    eprintln!("\n[error] {e}");
                } else if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "error", "content": e});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::CostStats {
                prompt_tokens,
                completion_tokens,
                turn_cost,
                cumulative_cost: cum_cost,
            } => {
                *total_prompt_tokens += prompt_tokens;
                *total_completion_tokens += completion_tokens;
                *cumulative_cost = *cum_cost;

                if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "cost",
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "turn_cost": turn_cost,
                        "cumulative_cost": *cum_cost,
                    });
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::PlanComplete => {
                // Non-interactive mode does not enter plan mode, so this
                // event should not arrive. If it does, ignore it.
            }
            session::executor::TurnEvent::Recovered { messages } => {
                if output == kirkforge::shared::OutputFormat::Text {
                    eprintln!("\n[recovered] restored {messages} message(s) from checkpoint");
                } else if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "recovered", "messages": messages});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::CompactionReport {
                dropped_tool_results,
                condensed_assistant_turns,
                original_count,
                compacted_count,
                tokens_before,
                tokens_after,
                new_messages: _,
            } => {
                if output == kirkforge::shared::OutputFormat::Text {
                    eprintln!(
                        "\n[compaction] {original_count} → {compacted_count} messages ({tokens_before} → {tokens_after} tokens), dropped {dropped_tool_results} tool result(s), condensed {condensed_assistant_turns} assistant turn(s).",
                    );
                } else if output == kirkforge::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "compaction",
                        "original_count": original_count,
                        "compacted_count": compacted_count,
                        "dropped_tool_results": dropped_tool_results,
                        "condensed_assistant_turns": condensed_assistant_turns,
                        "tokens_before": tokens_before,
                        "tokens_after": tokens_after,
                    });
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::PullProgress { .. } => {
                // Non-interactive mode has no place to show a live
                // progress bar; swallow the event silently.
            }
        }
    }
}

/// Resolve a `--continue-session` value to a log path.
///
/// Pure: takes the raw CLI string and returns either a `PathBuf`
/// (for path-style values) or an error. For id-prefix values, the
/// call to `session_index::resolve_session_id` is what actually
/// hits the filesystem — that side effect is documented at the
/// call site (`run_session`) so callers know what they're invoking.
pub(super) fn resolve_continue_path(value: &str) -> anyhow::Result<std::path::PathBuf> {
    if value.contains('/') || value.ends_with(".conv.ndjson") {
        return Ok(std::path::PathBuf::from(value));
    }
    match session::session_index::resolve_session_id(value) {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(anyhow::anyhow!(
            "No saved session found matching '{value}'. Run `kirkforge run --non-interactive` once to create one, or use `/sessions` in the TUI to list."
        )),
        Err(e) => Err(anyhow::anyhow!(
            "Error resolving session id '{value}': {e}"
        )),
    }
}
