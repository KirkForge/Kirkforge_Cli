/// Tool to cancel a running background bash job.
use crate::session::bash_jobs::global_registry;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;

pub struct BashCancel;

#[async_trait::async_trait]
impl Tool for BashCancel {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash_cancel",
            description: "Cancel a running background bash job by ID. Completed or already-failed jobs are unaffected.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "The job ID returned by bash with background=true"
                    }
                },
                "required": ["id"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let job_id = match args.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'id' argument".into(),
                }
            }
        };

        let registry = global_registry();
        if registry.cancel(job_id).await {
            ToolOutcome::Success {
                content: format!("Job #{} cancelled", job_id),
            }
        } else {
            match registry.get(job_id).await {
                Some(job) => ToolOutcome::Error {
                    message: format!("Job #{} is not running (status: {:?})", job_id, job.status),
                },
                None => ToolOutcome::Error {
                    message: format!("Job #{} not found", job_id),
                },
            }
        }
    }
}
