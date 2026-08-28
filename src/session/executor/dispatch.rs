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
    /// Owning subagent task id (WO 36.2) — lands in the per-call
    /// `ToolContext.task_owner` so background bash jobs are attributable.
    task_owner: Option<String>,
    /// Canonical run id (WO 45.1) — the session id, lands in the per-call
    /// `ToolContext.run_id` so spawned tasks and bash jobs carry it.
    run_id: Option<String>,
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
                // WO 47.19: recover from poison instead of skipping — a
                // panic that crossed this guard must not silently disable
                // bus verification for the rest of the session.
                let mut bus = bus_lock.lock().unwrap_or_else(|e| e.into_inner());
                bus.run(&ctx);
                for entry in bus.verdicts() {
                    let is_error = entry.severity == crate::session::verifier::bus::Severity::Error;
                    // WO 45.36: prior `success: !is_error` mapped Error
                    // → failure (false) and Info/Warning → success (true).
                    // Preserve that partition exactly with the typed
                    // outcome: Error → Failed; Info/Warning → Clean (the
                    // advisory findings were not verifier failures).
                    let outcome = if is_error {
                        crate::session::executor::types::VerificationOutcome::Failed
                    } else {
                        crate::session::executor::types::VerificationOutcome::Clean
                    };
                    corrections.push(CorrectionResult {
                        verifier: format!("{}", entry.source),
                        outcome,
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
                    let mut invocation = tc.clone();
                    // WO 38.1 TOCTOU: run the tool body against the Phase-1
                    // resolved (canonical) path, not the raw model argument.
                    // Previously the resolved path only surfaced at record
                    // time (turn.rs), so the body opened the ORIGINAL path —
                    // a same-batch bash call could swap a checked dir/file
                    // for a symlink between the guard check and the open.
                    if let Some(resolved) = &resolved {
                        if let Some(obj) = invocation.arguments.as_object_mut() {
                            obj.insert(
                                "path".into(),
                                serde_json::Value::String(resolved.to_string_lossy().into_owned()),
                            );
                        }
                    }
                    prepared.push(PreparedCall {
                        idx,
                        invocation,
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
                        task_owner: self.task_owner.clone(),
                        // WO 45.1: thread the session id as the canonical
                        // run_id so spawned tasks and bash jobs carry it.
                        // `None` when session_id is empty (tests, bench).
                        run_id: if self.session_id.is_empty() {
                            None
                        } else {
                            Some(self.session_id.clone())
                        },
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
    /// already recorded via `record_tool_result` (the mid-batch persistence
    /// guarantee — each tool result is appended + sync_all'd to the NDJSON
    /// log before the next handle is awaited, so a crash while a later,
    /// slower tool is still in-flight does not lose results that already
    /// finished).
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
            // Emit ToolStart at dispatch time so the TUI has a streaming
            // card to append PTY chunks to before the body finishes. The
            // record-time emissions in `record_tool_result` were removed
            // (WO 44.38) — they fired after the body ran, so PTY chunks
            // flowing during the body had no card to land in.
            crate::emit!(
                event_tx,
                TurnEvent::ToolStart {
                    name: prep.invocation.name.clone(),
                    args: prep.invocation.arguments.clone(),
                }
            );
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
        // each completed tool result is appended to the conversation log
        // (NDJSON append + sync_all) before the next handle is awaited. This
        // is the "mid-batch persistence" guarantee — a crash or cancellation
        // while a later, slower tool is still in-flight does not lose the
        // results that already finished. WO 38.9: the per-tool full-file
        // checkpoint rewrite was removed (O(N²)); the NDJSON append is
        // already crash-safe, and post-batch/post-turn checkpoints remain.
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
            // WO 38.9: per-tool checkpoint removed. The NDJSON append
            // in record_tool_result already persists each tool result
            // with sync_all — crash-safe. The full-file checkpoint
            // rewrite was O(N²) (clone + rewrite per tool call). Post-
            // batch (turn.rs) and post-turn checkpoints remain for
            // crash recovery.
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
            // WO 43.16 no-throw: `pre_run_verdict` guarantees file tools
            // return `Spawn(tool, Some(resolved))` — a `None` here means
            // the invariant broke. Previously an `expect` panic; now a
            // guarded Failure(Internal) so a dispatch bug becomes a tool
            // error the model can react to, not an executor unwind.
            let Some(path) = prep.resolved_path.as_ref().cloned() else {
                let invocation = prep.invocation.clone();
                results.insert(
                    idx,
                    (
                        invocation,
                        ToolOutcome::Failure(crate::shared::ToolError::Internal {
                            message: format!(
                                "file call '{name}' reached Phase 2.5 without a resolved path"
                            ),
                        }),
                        None,
                        0,
                    ),
                );
                continue;
            };

            // WO 38.1 / 44.28: re-verify no component of the resolved path became
            // a symlink after Phase-1 canonicalization. A same-batch bash call can
            // swap a dir or file for a symlink in the check-to-open window. The
            // walk runs unconditionally for every deferred file call (read_file,
            // read_image, write_file, edit_file, notebook_edit) so it covers the
            // read-before-edit
            // gate's Allowed arm too — pre-44.28 it only ran inside the Denied
            // arm, so the attack's exact precondition (file pre-read, gate allows)
            // bypassed it.
            // ponytail: the stat-walk is not atomic with the body's open — a
            // swap inside that micro-window still slips through. The upgrade
            // path is openat2(RESOLVE_NO_SYMLINKS) (or per-component openat
            // with O_NOFOLLOW) at the tool-body open site.
            if let Some(msg) = symlink_swap_denied(&path) {
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

            let path_arg = prep
                .invocation
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // notebook_edit only ever modifies an existing notebook (its body
            // fails on a missing file), so — like edit_file — it always needs
            // the read gate; write_file only needs it when overwriting.
            let needs_read_gate = name == "edit_file"
                || name == "notebook_edit"
                || (name == "write_file" && path.exists());
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
            // Emit ToolStart at dispatch time (WO 44.38) — same rationale
            // as the non-file arm above. File tools don't stream PTY chunks
            // but the TUI still shows a streaming card until ToolResult
            // finalizes it, and `run_turn_collecting` pairs the event.
            crate::emit!(
                event_tx,
                TurnEvent::ToolStart {
                    name: name.clone(),
                    args: prep.invocation.arguments.clone(),
                }
            );
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
                // Already recorded and appended in the collect loop.
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
                    crate::emit!(event_tx, ev);
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
                crate::emit!(
                    event_tx,
                    TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: err.clone(),
                        success: false,
                    }
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
                    "write_file" | "edit_file" | "notebook_edit" | "bash" | "read_file"
                );
                if is_destructive {
                    self.audit_log
                        .log_destructive(&tc.name, &tc.arguments, false, Some(message));
                }
                crate::emit!(
                    event_tx,
                    TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: message.clone(),
                        success: false,
                    }
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
                // WO 38.9: per-tool checkpoint removed — see spawn_batch.
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

            // WO 38.9: per-tool checkpoint removed — see spawn_batch.
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
        task_owner: prep.task_owner.clone(),
        // WO 45.1: thread the canonical run id (the session id) so
        // spawned tasks and bash jobs carry it for replay/audit/cancel.
        run_id: prep.run_id.clone(),
        event_tx: prep.event_tx,
    };
    let start = Instant::now();
    // catch_unwind so a panicking tool returns a clean Failure outcome
    // instead of unwinding through the executor loop — in unwind builds
    // (dev/test). In release ([profile.release] panic = "abort",
    // Cargo.toml) this guard never fires: the process aborts, and the
    // WO 38.2 panic hook (install_panic_hook, tui/mod.rs) restores the
    // terminal before the abort (WO 47.23 contract). This matters for
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

/// Return `Some(reason)` if any component of `resolved` is a symlink.
/// `resolved` is the Phase-1 canonical path — its components were real
/// directories when canonicalized, so any symlink found now was swapped
/// in after validation (WO 38.1 symlink TOCTOU).
///
/// `pub(crate)` for the verifier correction loop (WO 47.19): its
/// auto-fix write path runs outside dispatch and needs the same walk.
pub(crate) fn symlink_swap_denied(resolved: &std::path::Path) -> Option<String> {
    let mut acc = std::path::PathBuf::new();
    for comp in resolved.components() {
        acc.push(comp.as_os_str());
        if let Ok(md) = std::fs::symlink_metadata(&acc) {
            if md.file_type().is_symlink() {
                return Some(format!(
                    "path component '{}' was replaced by a symlink after validation",
                    acc.display()
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::symlink_swap_denied;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // WO 43.12: ungated — pure std::fs, no Unix-only API. WO 44.28: per-call
    // unique dir — the shared name raced under parallel test runners (one
    // test's `remove_dir_all` wiped another's `victim.txt` mid-canonicalize).
    static TEMP_ROOT_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_root() -> std::path::PathBuf {
        let n = TEMP_ROOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kf_wo38_symlink_walk_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // WO 43.12: ungated — symlink_swap_denied uses std::fs::symlink_metadata
    // + file_type().is_symlink() (both cross-platform); no symlink is created
    // here, so the test runs identically on Windows.
    #[test]
    fn symlink_swap_denied_allows_real_path() {
        let dir = temp_root();
        let file = dir.join("real.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(symlink_swap_denied(&file.canonicalize().unwrap()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Final component swapped for a symlink after validation → deny.
    /// This is the read-side `.ssh` swap case from the audit.
    #[cfg(unix)]
    #[test]
    fn symlink_swap_denied_blocks_swapped_file() {
        let dir = temp_root();
        let target = dir.join("secret.txt");
        let file = dir.join("victim.txt");
        std::fs::write(&target, "secret").unwrap();
        std::fs::write(&file, "harmless").unwrap();
        let resolved = file.canonicalize().unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        let verdict = symlink_swap_denied(&resolved);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            verdict.as_ref().is_some_and(|m| m.contains("symlink")),
            "swapped final component must be denied, got {verdict:?}"
        );
    }

    /// Parent component swapped for a symlink after validation → deny.
    #[cfg(unix)]
    #[test]
    fn symlink_swap_denied_blocks_swapped_parent_dir() {
        let dir = temp_root();
        let outside = dir.join("outside");
        let inside = dir.join("sandbox");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(inside.join("sub")).unwrap();
        let file = inside.join("sub").join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let resolved = file.canonicalize().unwrap();
        std::fs::remove_dir_all(inside.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, inside.join("sub")).unwrap();
        let verdict = symlink_swap_denied(&resolved);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            verdict.as_ref().is_some_and(|m| m.contains("symlink")),
            "swapped parent dir must be denied, got {verdict:?}"
        );
    }

    /// Write case: new file whose final component does not exist yet —
    /// NotFound is not a symlink, so the walk passes.
    // WO 43.12: ungated — no symlink created; pure std::path check.
    #[test]
    fn symlink_swap_denied_allows_nonexistent_new_file() {
        let dir = temp_root();
        let resolved = dir.join("brand_new.txt");
        assert!(symlink_swap_denied(&resolved).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
