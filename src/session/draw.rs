//! In-process Draw tool wrapper.
//!
//! When the `draw` feature is enabled, this module provides a `draw_render`
//! tool that loads a `.td.json` file and renders it as plain text using
//! `kirkforge_draw_core` directly, eliminating subprocess overhead.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};

pub struct DrawRenderTool;

#[async_trait::async_trait]
impl Tool for DrawRenderTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "draw_render",
            description: "Render a .td.json terminal diagram file as plain text. Returns fenced markdown output.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the .td.json file to render"
                    },
                    "fenced": {
                        "type": "boolean",
                        "description": "Wrap output in a markdown fenced code block (default: true)",
                        "default": true
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "draw_render: missing 'path' argument",
                ));
            }
        };

        let fenced = args.get("fenced").and_then(|v| v.as_bool()).unwrap_or(true);

        let expanded = shellexpand::tilde(&path).to_string();
        let json = match std::fs::read_to_string(&expanded) {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("draw_render: cannot read {expanded}: {e}"),
                });
            }
        };

        let (doc, report) = match kirkforge_draw_core::load_document(&json) {
            Ok(pair) => pair,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("draw_render: failed to parse {expanded}: {e}"),
                });
            }
        };

        let warnings = report.unknown_object_warnings;
        let rendered = kirkforge_draw_core::render_plain(&doc);

        if fenced {
            let mut out = String::from("```\n");
            out.push_str(&rendered);
            out.push_str("```");
            if !warnings.is_empty() {
                out.push_str("\n\nWarnings:\n");
                for w in &warnings {
                    out.push_str(&format!("- {w}\n"));
                }
            }
            ToolOutcome::Success { content: out }
        } else {
            let mut out = rendered;
            if !warnings.is_empty() {
                out.push_str("\nWarnings:\n");
                for w in &warnings {
                    out.push_str(&format!("- {w}\n"));
                }
            }
            ToolOutcome::Success { content: out }
        }
    }
}

/// Return the draw tool as a trait object.
pub fn draw_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![std::sync::Arc::new(DrawRenderTool)]
}
