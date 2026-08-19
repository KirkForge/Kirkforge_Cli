//! Tool-call dispatch and verifier correction emission.
//!
//! Hosts the three phases of `dispatch_tool_call_batch`:
//! - `prepare_batch` (Phase 1): per-call pre-gate via `pre_run_verdict`.
//! - `spawn_batch`   (Phase 2 + 2.5): run/spawn non-file calls, then file calls sequentially.
//! - `collect_batch` (Phase 3): record results in input order.
//!
//! `turn.rs::dispatch_tool_call_batch` is now a ≤30-line orchestrator.

use crate::session::access::GuardVerdict;
use crate::session::verifier::CorrectionResult;
use crate::shared::{read_shared_config, Message, Role, ToolInvocation, ToolOutcome};

use super::helpers::tool_cancel_token;
use super::pre_run::PreRunVerdict;
use super::types::TurnEvent;
use super::{ApprovalRequest, Executor};

use futures_util::future::FutureExt;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

type RunningTask = (
    usize,
    tokio::task::JoinHandle<Option<(ToolInvocation, ToolOutcome, u64)>>,
);

/// Inputs cloned for a single prepared tool call. Owned so the call can be
/// moved into a spawned task without borrowing `Executor` state.
pub(super) struct PreparedCall {
    idx: usize,
    invocation: ToolInvocation,
    tool: Arc<dyn crate::tools::Tool>,
    cancel_token: tokio_util::sync::CancellationToken,
    resolved_path: Option<std::path::PathBuf>,
    timeout: std::time::Duration,
    diff_review: bool,
    event_tx: Option<mpsc::Sender<TurnEvent>>,
    /// The session's task spawner, threaded through so the `task` tool can
    /// reach it via `ctx.task_spawner` (WO 30.6). Previously this was None
    /// and the parent's task tool always errored "not available".
    task_spawner: Option<Arc<dyn crate::tools::task::TaskSpawner>>,
}

/// Phase-1 output: a buffered skip (denied/unknown tool/plan-mode/etc.) waiting
/// to be replayed in input order during Phase 3.
pub(super) type SkippedCall = (usize, ToolInvocation, Vec<TurnEvent>, String);

/// Phase-2 output: completed tool bodies keyed by input index. The third
/// element is the resolved path Phase 1 already sandbox-checked; Phase 3 reuses
/// it instead of re-running the path guard (closes the WO 15.9 TOCTOU + double
/// `git check-ignore` window). Non-file tools store `None`.
pub(super) type ToolResult = (ToolInvocation, ToolOutcome, Option<std::path::PathBuf>, u64);

impl Executor {
    // reason: bash metrics (exit/stdout/stderr) + edit diff are independent optional payloads.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn emit_tool_event_and_correct(
        &self,
        _tc: &ToolInvocation,
        tool_name: &str,
        args: &serde_json::Value,
        outcome: &ToolOutcome,
        real_exit_code: Option<i32>,
        real_stdout_len: Option<usize>,
        real_stderr_len: Option<usize>,
        edit_diff: Option<String>,
    ) -> Vec<CorrectionResult> {
        use crate::session::verifier::types::*;

        let bus_event = match tool_name {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::FileRead(FileReadEvent {
                    path: std::path::PathBuf::from(&path),
                    size_bytes: 0,
                    truncated: false,
                }))
            }
            "write_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                content.hash(&mut hasher);
                Some(BusEvent::FileWrite(FileWriteEvent {
                    path: std::path::PathBuf::from(&path),
                    content_length: content.len(),
                    content_hash: hasher.finish(),
                }))
            }
            "edit_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let diff = edit_diff.unwrap_or_else(|| {
                    args.get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                });
                Some(BusEvent::Edit(EditEvent {
                    path: std::path::PathBuf::from(&path),
                    diff,
                }))
            }
            "bash" => {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let workdir = args
                    .get("workdir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from);
                Some(BusEvent::BashExec(BashExecEvent {
                    command,
                    exit_code: real_exit_code.unwrap_or(0),
                    stdout_len: real_stdout_len.unwrap_or(0),
                    stderr_len: real_stderr_len.unwrap_or(0),
                    workdir,
                }))
            }
            _ => None,
        };

        let error_event = match outcome {
            ToolOutcome::Error { message } => Some(BusEvent::ToolError(ToolErrorEvent {
                tool: tool_name.to_string(),
                error: message.clone(),
            })),
            ToolOutcome::Failure(err) => Some(BusEvent::ToolError(ToolErrorEvent {
                tool: tool_name.to_string(),
                error: err.to_user_message(),
            })),
            _ => None,
        };

        let mut corrections = Vec::new();

        if let Some(ref event) = bus_event {
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        if let Some(ref event) = error_event {
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        // Run the unified verifier bus after file-modifying tool calls.
        let is_file_modification = matches!(tool_name, "write_file" | "edit_file");
        if is_file_modification {
            if let Some(ref bus_lock) = self.verifier_bus {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let ctx = crate::session::verifier::bus::VerifyContext {
                    sandbox_dir: self
                        .sandbox
                        .path_guard
                        .sandbox_dir
                        .clone()
                        .unwrap_or_default(),
                    changed_files: vec![std::path::PathBuf::from(path)],
                };
                if let Ok(mut bus) = bus_lock.lock() {
                    bus.run(&ctx);
                    for entry in bus.verdicts() {
                        let is_error =
                            entry.severity == crate::session::verifier::bus::Severity::Error;
                        corrections.push(CorrectionResult {
                            verifier: format!("{}", entry.source),
                            success: !is_error,
                            message: format!(
                                "[{}] {}:{} {}",
                                entry.severity,
                                entry
                                    .file
                                    .as_ref()
                                    .map(|f| f.display().to_string())
                                    .unwrap_or_else(|| "—".to_string()),
                                entry
                                    .line
                                    .map(|l| l.to_string())
                                    .unwrap_or_else(|| "—".to_string()),
                                entry.message
                            ),
                            fix: None,
                            file: entry.file.clone(),
                            line: entry.line,
                        });
                    }
                    bus.clear();
                }
            }
        }

        corrections
    }

    /// Phase 1 — Prepare + pre-gate. Determine, for each call, whether it
    /// should be spawned or skipped with a buffered failure. This phase does
    /// not mutate Executor state (it only reads config/tools/guards).
    pub(super) async fn prepare_batch(
        &mut self,
        tcs: &[ToolInvocation],
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<(Vec<PreparedCall>, Vec<SkippedCall>)> {
        let mut prepared: Vec<PreparedCall> = Vec::with_capacity(tcs.len());
        let mut skipped: Vec<SkippedCall> = Vec::new();
        for (idx, tc) in tcs.iter().enumerate() {
            match self.pre_run_verdict(tc, approval_sender).await? {
                PreRunVerdict::Spawn(tool, resolved) => {
                    prepared.push(PreparedCall {
                        idx,
                        invocation: tc.clone(),
                        tool,
                        // WO 35.3: when a root cancel token is attached
                        // (subagent executors), per-call tokens are LIVE
                        // children — an external cancel fires them mid-run
                        // and a bash child dies immediately instead of at
                        // `tool_timeout_secs`. Parent sessions keep the
                        // snapshot-at-dispatch semantics (WO 15.7).
                        cancel_token: self.cancel_token.as_ref().map_or_else(
                            || tool_cancel_token(cancelled),
                            |root| root.child_token(),
                        ),
                        resolved_path: resolved,
                        timeout: self.tool_call_timeout(),
                        diff_review: read_shared_config(&self.config).security.diff_review,
                        event_tx: Some(event_tx.clone()),
                        task_spawner: self
                            .task_spawner
                            .clone()
                            .map(|s| s as Arc<dyn crate::tools::task::TaskSpawner>),
                    });
                }
                PreRunVerdict::Skip { events, message } => {
                    skipped.push((idx, tc.clone(), events, message));
                }
            }
        }
        Ok((prepared, skipped))
    }

    /// Phase 2 — Run: spawn one task per non-file call. File calls are
    /// run sequentially in Phase 2.5 so the read-before-edit gate can observe
    /// reads before edits in the same batch.
    ///
    /// When deterministic mode is active (--seed), skip tokio::spawn and
    /// run all calls sequentially to eliminate nondeterminism from task
    /// scheduling. The tool-call *sequence* is what matters for regression
    /// testing; the model's output content may still vary by provider.
    ///
    /// Returns the results map (keyed by input index) and the set of indices
    /// already recorded + checkpointed via `record_tool_result` (the mid-batch
    /// checkpoint guarantee — a crash while a later, slower tool is still
    /// in-flight does not lose results that already finished).
    pub(super) async fn spawn_batch(
        &mut self,
        tcs: &mut [ToolInvocation],
        prepared: Vec<PreparedCall>,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<(HashMap<usize, ToolResult>, HashSet<usize>)> {
        let mut running: Vec<RunningTask> = Vec::with_capacity(prepared.len());
        let mut deferred_file_calls: Vec<PreparedCall> = Vec::new();
        let mut results: HashMap<usize, ToolResult> = HashMap::with_capacity(prepared.len());
        let deterministic = self.is_deterministic();
        for prep in prepared {
            if prep.resolved_path.is_some() {
                deferred_file_calls.push(prep);
                continue;
            }
            let idx = prep.idx;
            if deterministic {
                // Run sequentially — no tokio::spawn, no concurrency.
                let outcome = run_prepared_call(prep).await;
                if let Some((invocation, result, ms)) = outcome {
                    results.insert(idx, (invocation, result, None, ms));
                }
            } else {
                let handle = tokio::spawn(run_prepared_call(prep));
                running.push((idx, handle));
            }
            // Yield so the just-spawned task can start. If the user cancelled
            // while we were pre-gating, the next iteration sees the flag and
            // stops spawning remaining calls. This preserves the sequential
            // cancellation semantics tests rely on without losing concurrency
            // among the calls that were already spawned.
            tokio::task::yield_now().await;
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
        }

        // Collect completed non-file results and record them incrementally.
        //
        // Recording is interleaved with collection (not deferred to Phase 3):
        // each completed tool result is appended to the conversation and
        // checkpointed before the next handle is awaited. This is the
        // "mid-batch checkpoint" guarantee — a crash or cancellation while a
        // later, slower tool is still in-flight does not lose the results
        // that already finished. Without this, a turn aborted while the
        // collect loop is blocked on a slow tool would persist nothing,
        // because Phase 3 (which appends to the conversation) only runs
        // after the whole collect loop completes.
        //
        // Handles are awaited in input order so the conversation still
        // records tool results in the order the model requested them.
        // Cancellation is checked after each record: once cancelled, the
        // remaining in-flight handles are dropped (their tasks finish
        // detached but their results are never recorded) and Phase 3 / the
        // caller append placeholder tool-result messages for them.
        //
        // In deterministic mode (--seed), non-file tools ran sequentially
        // in Phase 2 and their results are already in `results`. The
        // `running` vec is empty, so this loop is a no-op.
        let mut recorded: HashSet<usize> = HashSet::with_capacity(running.len());
        // Await handles in input order (front-to-back) so the conversation
        // records tool results in the order the model requested them. On
        // cancellation, abort the remaining un-awaited handles so they do
        // not run detached holding subprocess/network resources for up to
        // `tool_timeout_secs` (WO 15.7 2.3 — cancel leak: a dropped
        // `JoinHandle` detaches its task instead of stopping it).
        let mut iter = running.drain(..);
        while let Some((idx, handle)) = iter.next() {
            let pair = if let Ok(Some(p)) = handle.await {
                p
            } else {
                // Join error (task panicked/cancelled): leave unrecorded so
                // Phase 3 emits a placeholder for this index.
                continue;
            };
            let tc = &mut tcs[idx];
            let (invocation, outcome, duration_ms) = pair;
            self.record_tool_result(
                tc,
                &invocation,
                outcome,
                None,
                duration_ms,
                approval_sender,
                cancelled,
                event_tx,
            )
            .await?;
            if let Err(e) = self.conversation.checkpoint_async().await {
                tracing::warn!(error = %e, "mid-batch checkpoint failed after tool {}", tc.id);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
            recorded.insert(idx);
            // Stop awaiting further handles once cancelled; the in-flight
            // task we just awaited is recorded, later ones get placeholders.
            // Abort the remaining un-awaited handles (WO 15.7 2.3).
            if cancelled.load(Ordering::SeqCst) {
                for (_, h) in iter {
                    h.abort();
                }
                break;
            }
        }

        // Phase 2.5 — Run file tools sequentially in input order. Cancellation
        // is checked before each call. The read-before-edit gate is checked
        // before running a write/edit body so unread existing files are never
        // touched; reads earlier in the same batch have already been marked at
        // the end of their body, so `[read_file(X), write_file(X)]` passes.
        for prep in deferred_file_calls {
            if cancelled.load(Ordering::SeqCst) {
                // The remaining deferred file calls won't be recorded;
                // Phase 3 will append placeholders for them.
                break;
            }
            let idx = prep.idx;
            let name = prep.invocation.name.clone();
            let path = prep
                .resolved_path
                .as_ref()
                .expect("file call has resolved path")
                .clone();

            let path_arg = prep
                .invocation
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let needs_read_gate = name == "edit_file" || (name == "write_file" && path.exists());
            if needs_read_gate {
                if let GuardVerdict::Denied(msg) = self
                    .sandbox
                    .check_edit(std::path::Path::new(path_arg), &path)
                {
                    let denied = format!("🔒 Access denied: {msg}");
                    let invocation = prep.invocation.clone();
                    results.insert(
                        idx,
                        (
                            invocation,
                            ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                                message: denied,
                            }),
                            Some(path.clone()),
                            0,
                        ),
                    );
                    continue;
                }
            }

            let invocation = prep.invocation.clone();
            let outcome = run_prepared_call(prep).await.map(|(_, o, ms)| (o, ms));
            if let Some((ref o, ms)) = outcome {
                // Mark reads immediately so later writes in the same batch
                // see them when their read-before-edit gate runs.
                if name == "read_file" || name == "read_image" {
                    self.sandbox.mark_read(&path);
                }
                results.insert(idx, (invocation, o.clone(), Some(path.clone()), ms));
            }
        }

        Ok((results, recorded))
    }

    /// Phase 3 — Record: walk input order, replay skipped/denied calls,
    /// then record each completed file-tool result in order. Non-file
    /// results were already recorded incrementally in `spawn_batch`'s collect
    /// loop (so a mid-batch crash persists them); skip those indices here.
    /// Cancellation is checked only when a result is missing, so
    /// already-completed tool bodies are still recorded and earlier calls
    /// in the batch don't become placeholders.
    ///
    /// Returns the first index that was left unrecorded due to cancellation
    /// (for the caller's placeholder path), or `tcs.len()` on full success.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn collect_batch(
        &mut self,
        tcs: &mut [ToolInvocation],
        mut results: HashMap<usize, ToolResult>,
        mut skipped: Vec<SkippedCall>,
        recorded: &HashSet<usize>,
        cancelled: &AtomicBool,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<usize> {
        for (idx, tc) in tcs.iter_mut().enumerate() {
            if recorded.contains(&idx) {
                // Already recorded and checkpointed in the collect loop.
                continue;
            }
            let has_result = results.contains_key(&idx);
            let has_skip = skipped.iter().any(|(i, _, _, _)| *i == idx);
            if !has_result && !has_skip && cancelled.load(Ordering::SeqCst) {
                tracing::trace!("tool batch short-circuited by cancellation at record phase");
                return Ok(idx);
            }

            if let Some(pos) = skipped.iter().position(|(i, _, _, _)| *i == idx) {
                let (_, _inv, events, msg) = skipped.swap_remove(pos);
                for ev in events {
                    crate::send_or_warn!(
                        event_tx.send(ev).await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: msg,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                continue;
            }

            let Some((invocation, outcome, resolved_path, duration_ms)) = results.remove(&idx)
            else {
                let err = format!("Tool call {} did not return an outcome", tc.id);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: err.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: err,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                continue;
            };

            // Phase 2.5 already ran the read-before-edit gate and produced
            // an `AccessDenied` failure for deferred file calls. Re-running
            // `record_tool_result` here would re-check the path guard + read
            // gate and emit a second, identical "Access denied" message —
            // the model would see two denials for one failed edit (WO 15.7
            // 2.8). Record the pre-built denial once and skip the re-check.
            if let ToolOutcome::Failure(crate::shared::ToolError::AccessDenied { message }) =
                &outcome
            {
                let is_destructive = matches!(
                    tc.name.as_str(),
                    "write_file" | "edit_file" | "bash" | "read_file"
                );
                if is_destructive {
                    self.audit_log
                        .log_destructive(&tc.name, &tc.arguments, false, Some(message));
                }
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: message.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: message.clone(),
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                if let Err(e) = self.conversation.checkpoint_async().await {
                    tracing::warn!(error = %e, "mid-batch checkpoint failed after tool {}", tc.id);
                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                continue;
            }

            self.record_tool_result(
                tc,
                &invocation,
                outcome,
                resolved_path.as_deref(),
                duration_ms,
                approval_sender,
                cancelled,
                event_tx,
            )
            .await?;

            // Persist after each recorded result so a crash before the next
            // tool starts does not lose in-flight progress.
            if let Err(e) = self.conversation.checkpoint_async().await {
                tracing::warn!(error = %e, "mid-batch checkpoint failed after tool {}", tc.id);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }

        Ok(tcs.len())
    }
}

/// Run only the tool body for a prepared call, returning the original
/// invocation and the tool outcome.
///
/// This function deliberately does not touch `Executor` state; it is the
/// concurrency boundary where tool I/O may run in parallel. Tasks already
/// past the spawn point when the user cancels are stopped by aborting
/// their `JoinHandle` in the collect loop (WO 15.7 2.3) — a dropped
/// `JoinHandle` detaches the task instead of stopping it, so the collect
/// loop aborts the remaining un-awaited handles on cancellation.
async fn run_prepared_call(prep: PreparedCall) -> Option<(ToolInvocation, ToolOutcome, u64)> {
    // Short-circuit when the token was already cancelled at spawn time
    // (the `tool_cancel_token` helper snapshots the `cancelled` flag).
    if prep.cancel_token.is_cancelled() {
        return Some((
            prep.invocation,
            ToolOutcome::Failure(crate::shared::ToolError::Cancelled),
            0,
        ));
    }
    if let Err(msg) =
        crate::tools::validate_tool_args(prep.tool.as_ref(), &prep.invocation.arguments)
    {
        return Some((
            prep.invocation,
            ToolOutcome::Failure(crate::shared::ToolError::invalid_args(&msg)),
            0,
        ));
    }

    let ctx = crate::tools::ToolContext {
        token: prep.cancel_token,
        dry_run: false,
        diff_review: prep.diff_review,
        task_spawner: prep.task_spawner.clone(),
        tools: None,
        event_tx: prep.event_tx,
    };
    let start = Instant::now();
    // catch_unwind so a panicking tool returns a clean Failure outcome
    // instead of unwinding through the executor loop. This matters for
    // the direct-call paths (deterministic mode, Phase 2.5 deferred file
    // calls) which run run_prepared_call on the executor task rather than
    // in a spawned task. The spawned path also benefits: the panic message
    // is preserved in the Internal error rather than being discarded as a
    // JoinError.
    let outcome = match tokio::time::timeout(
        prep.timeout,
        AssertUnwindSafe(prep.tool.run(&ctx, prep.invocation.arguments.clone())).catch_unwind(),
    )
    .await
    {
        Err(_) => ToolOutcome::Failure(crate::shared::ToolError::Timeout {
            after_secs: prep.timeout.as_secs(),
        }),
        Ok(Err(panic_payload)) => {
            let msg = panic_payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::warn!("tool {} panicked: {msg}", prep.invocation.name);
            ToolOutcome::Failure(crate::shared::ToolError::Internal {
                message: format!("tool panicked: {msg}"),
            })
        }
        Ok(Ok(outcome)) => outcome,
    };
    let duration_ms = start.elapsed().as_millis() as u64;
    Some((prep.invocation, outcome, duration_ms))
}
