/// Tool to check the status of background bash jobs.
use crate::session::bash_jobs::global_registry;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;

pub struct BashStatus;

#[async_trait::async_trait]
impl Tool for BashStatus {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash_status",
            description: "Check the status of a background bash job by ID. Returns the job's current status (running/completed/failed/cancelled) and any output captured so far.",
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
        match registry.get(job_id).await {
            Some(job) => {
                let status_label = match &job.status {
                    crate::session::bash_jobs::JobStatus::Running => "running",
                    crate::session::bash_jobs::JobStatus::Completed(code) => {
                        return ToolOutcome::Success {
                            content: format!(
                                "Job #{} completed (exit code {})\nstdout:\n{}\nstderr:\n{}",
                                job.id, code, job.stdout, job.stderr
                            ),
                        };
                    }
                    crate::session::bash_jobs::JobStatus::Failed(e) => {
                        return ToolOutcome::Error {
                            message: format!("Job #{} failed: {}", job.id, e),
                        };
                    }
                    crate::session::bash_jobs::JobStatus::Cancelled => "cancelled",
                };
                ToolOutcome::Success {
                    content: format!(
                        "Job #{} is {}\ncommand: {}\n---\nstdout so far:\n{}\nstderr so far:\n{}",
                        job.id, status_label, job.command, job.stdout, job.stderr
                    ),
                }
            }
            None => ToolOutcome::Error {
                message: format!("Job #{job_id} not found"),
            },
        }
    }
}
