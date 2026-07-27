/// Tool to cancel a running background bash job.
use crate::session::bash_jobs::global_registry;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};

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

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let job_id = match args.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'id' argument"));
            }
        };

        let registry = global_registry();
        if registry.cancel(job_id).await {
            ToolOutcome::Success {
                content: format!("Job #{job_id} cancelled"),
            }
        } else {
            match registry.get(job_id).await {
                Some(job) => ToolOutcome::Failure(ToolError::Execution {
                    message: format!("Job #{} is not running (status: {:?})", job_id, job.status),
                    exit_code: None,
                    stderr: String::new(),
                }),
                None => ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Job #{job_id} not found"),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_cancel_def_has_correct_name() {
        let tool = BashCancel;
        let def = tool.def();
        assert_eq!(def.name, "bash_cancel");
    }

    #[test]
    fn bash_cancel_def_has_id_parameter() {
        let tool = BashCancel;
        let def = tool.def();
        let params = def.parameters.as_object().unwrap();
        assert!(params.get("properties").unwrap().get("id").is_some());
        assert!(params
            .get("required")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("id")));
    }

    #[tokio::test]
    async fn bash_cancel_missing_id_returns_failure() {
        let tool = BashCancel;
        let ctx = ToolContext::default();
        let result = tool.run(&ctx, serde_json::json!({})).await;
        assert!(matches!(result, ToolOutcome::Failure(_)));
    }

    #[tokio::test]
    async fn bash_cancel_nonexistent_job_returns_failure() {
        let tool = BashCancel;
        let ctx = ToolContext::default();
        let result = tool.run(&ctx, serde_json::json!({"id": 99999})).await;
        assert!(matches!(result, ToolOutcome::Failure(_)));
    }
}
