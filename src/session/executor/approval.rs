//! Approval request/response types and flow.

use crate::shared::metrics::{record, MetricEvent};
use crate::shared::permission::push_rule_unique;
use crate::shared::ToolInvocation;
use std::time::Duration;
use tokio::sync::mpsc;

use super::types::ApprovalDecision;
use super::Executor;

/// Maximum time to wait for a user approval decision. Prevents a hung UI
/// or mis-wired handler from blocking the executor forever.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response: ApprovalResponder,
}

/// Wrapper around the executor's oneshot approval response sender.
///
/// The oneshot channel is the boundary between the executor (which waits on
/// `response_rx.await`) and the TUI / line-mode handler (which waits for the
/// user). If the handler drops the `Sender` without calling `send`, the
/// executor sees `oneshot::error::RecvError` and surfaces the confusing
/// "Approval channel closed" message. This wrapper guarantees that the
/// sender is always consumed with a response: either the explicit one from
/// the user, or `ApprovalResponse::Denied` if the pending approval is dropped
/// (app shutdown, render error, superseded request, etc.).
#[derive(Debug)]
pub struct ApprovalResponder {
    inner: Option<tokio::sync::oneshot::Sender<ApprovalResponse>>,
}

impl ApprovalResponder {
    pub fn new(tx: tokio::sync::oneshot::Sender<ApprovalResponse>) -> Self {
        Self { inner: Some(tx) }
    }

    /// Consume this responder and send `resp` back to the executor.
    /// Returns `Ok(())` on success, or `Err` if the executor's receiver is
    /// already gone (cancelled / shut down).
    pub fn send(mut self, resp: ApprovalResponse) -> Result<(), ApprovalResponse> {
        if let Some(tx) = self.inner.take() {
            tx.send(resp)
        } else {
            // Already consumed or dropped once; shouldn't happen in normal
            // use, but treat it as a closed channel by returning the value.
            Err(resp)
        }
    }
}

impl Drop for ApprovalResponder {
    fn drop(&mut self) {
        if let Some(tx) = self.inner.take() {
            // Always answer so the executor never blocks forever and never
            // sees the generic "channel closed" error. Use a reasoned denial
            // so the model (and user) can tell this was not an explicit
            // user decision — it happened because the handler dropped the
            // responder without sending a response (e.g. app shutdown, render
            // panic, or a handler bug).
            if tx
                .send(ApprovalResponse::DeniedWithReason(
                    "approval responder dropped without a user decision".into(),
                ))
                .is_err()
            {
                tracing::debug!("approval fallback response receiver already dropped");
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalResponse {
    Approved,
    Denied,
    /// Denied with an explanatory message surfaced to the model as the
    /// tool result. Used by non-interactive mode so the agent knows why
    /// a destructive operation was rejected.
    DeniedWithReason(String),
    AlwaysApprove,
}

impl Executor {
    pub(crate) async fn run_approval_flow(
        &mut self,
        tc: &ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
    ) -> anyhow::Result<ApprovalDecision> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
        approval_sender
            .send(ApprovalRequest {
                tool_name: tc.name.clone(),
                args: tc.arguments.clone(),
                response: ApprovalResponder::new(response_tx),
            })
            .map_err(|_| anyhow::anyhow!("approval channel closed"))?;

        let decision = match tokio::time::timeout(APPROVAL_TIMEOUT, response_rx).await {
            Ok(Ok(ApprovalResponse::Approved)) => ApprovalDecision::Approved,
            Ok(Ok(ApprovalResponse::Denied)) => ApprovalDecision::Denied {
                reason: "User denied this operation".into(),
            },
            Ok(Ok(ApprovalResponse::DeniedWithReason(reason))) => {
                ApprovalDecision::Denied { reason }
            }
            Ok(Ok(ApprovalResponse::AlwaysApprove)) => {
                let rule = crate::shared::permission::suggest_rule(&tc.name, &tc.arguments);
                if let Ok(mut cfg) = self.config.write() {
                    push_rule_unique(&mut cfg.security.permission_rules, rule);
                }
                ApprovalDecision::AlwaysApproved
            }
            Ok(Err(_)) => ApprovalDecision::Denied {
                reason: "Approval channel closed".into(),
            },
            Err(_) => ApprovalDecision::Denied {
                reason: "Approval timed out after 5 minutes".into(),
            },
        };
        record(MetricEvent::Approval {
            action: match &decision {
                ApprovalDecision::Approved => "approved".to_string(),
                ApprovalDecision::Denied { .. } => "denied".to_string(),
                ApprovalDecision::AlwaysApproved => "always_approved".to_string(),
            },
        });
        Ok(decision)
    }
}
