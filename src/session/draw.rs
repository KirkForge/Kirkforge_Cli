//! In-process Draw tool wrapper.
//!
//! When the `draw` feature is enabled, this module provides a `draw_render`
//! tool that loads a `.td.json` file and renders it as plain text using
//! `kirkforge_draw_core` directly, eliminating subprocess overhead.

use crate::session::hooks::{HookContext, HookDecision, InProcessHook};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::Path;

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

/// In-process `post-turn` hook: nudges the model to render any new
/// `.td.json` files in the working directory. Mirrors the shell hook in
/// `plugins/kirkforge-draw/hooks/post-turn.sh`.
pub struct DrawPostTurnHook;

impl InProcessHook for DrawPostTurnHook {
    fn event(&self) -> &str {
        "post-turn"
    }

    fn handle(&self, _ctx: &HookContext) -> HookDecision {
        let mut hits: Vec<String> = Vec::new();
        collect_td_json(Path::new("."), &mut hits);
        collect_td_json(Path::new("./out"), &mut hits);

        if hits.is_empty() {
            return HookDecision::Allow;
        }

        hits.sort();
        hits.dedup();
        let count = hits.len();
        let display: Vec<String> = hits.iter().take(5).cloned().collect();
        let mut names = display.join(",");
        if count > 5 {
            names.push_str(", ...");
        }
        tracing::info!(
            count,
            "Found new .td.json: {names}. Render with kfd --load <path> --render --fenced if useful."
        );
        HookDecision::Allow
    }
}

fn collect_td_json(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".td.json") {
            let display = dir.join(name).to_string_lossy().replace('\\', "/");
            out.push(display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::hooks::{HookContext, HookDecision};
    use crate::tools::ToolContext;

    const MINIMAL_TD_JSON: &str = r#"{
  "version": 1,
  "objects": [
    {
      "type": "box",
      "id": "b1",
      "z": 1,
      "parentId": null,
      "color": "green",
      "left": 0, "top": 0, "right": 10, "bottom": 2,
      "style": "light"
    }
  ]
}"#;

    struct CwdGuard {
        prev: std::path::PathBuf,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let prev = std::env::current_dir().expect("current_dir");
            std::env::set_current_dir(dir).expect("set_current_dir");
            Self { prev }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }

    #[tokio::test]
    async fn test_draw_render_loads_and_renders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("diag.td.json");
        std::fs::write(&file_path, MINIMAL_TD_JSON).expect("write td.json");

        let tool = DrawRenderTool;
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": file_path.display().to_string(),
            "fenced": true,
        });
        let out = tool.run(&ctx, args).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("```"),
                    "fenced render must contain a code fence, got: {content}"
                );
                assert!(!content.is_empty(), "rendered output must be non-empty");
            }
            other => panic!("DrawRenderTool must return Success, got {other:?}"),
        }
    }

    #[test]
    fn test_post_turn_hook_finds_td_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.td.json"), MINIMAL_TD_JSON).expect("write a");
        std::fs::write(dir.path().join("b.td.json"), MINIMAL_TD_JSON).expect("write b");

        let _guard = CwdGuard::enter(dir.path());
        let hook = DrawPostTurnHook;
        let ctx = HookContext {
            event: "post-turn".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), HookDecision::Allow);
    }

    #[test]
    fn test_post_turn_hook_no_files_returns_allow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = CwdGuard::enter(dir.path());
        let hook = DrawPostTurnHook;
        let ctx = HookContext {
            event: "post-turn".into(),
            ..Default::default()
        };
        assert_eq!(
            hook.handle(&ctx),
            HookDecision::Allow,
            "DrawPostTurnHook with no .td.json files must still Allow"
        );
    }
}
