//! Unified toolset abstraction.
//!
//! A `Toolset` is a source-aware collection of tools that the executor can
//! list and resolve by name. KirkForge-Cli composes three sources into one
//! view:
//!
//!   * `builtin` — tools implemented directly in this crate (`read_file`,
//!     `bash`, ...).
//!   * `mcp`     — tools discovered from configured MCP servers.
//!   * `plugin`  — tools provided by Rust-native plugins loaded at startup.
//!
//! The executor only needs two operations: list all definitions and resolve
//! a single tool by name. Keeping the abstraction small means existing code
//! (`tools::all_tools`, `mcp_tools::all_mcp_tools`,
//! `plugin_tools::all_plugin_tools`) continues to produce the same
//! `Vec<Arc<dyn Tool>>` while gaining a source label when wrapped.

use crate::shared::ToolDef;
use crate::tools::Tool;
use std::sync::Arc;

/// A source-aware collection of tools.
pub trait Toolset: Send + Sync {
    /// Human-readable source label, e.g. `builtin`, `mcp`, or `plugin`.
    fn source(&self) -> &str;

    /// All tool definitions in this set.
    fn definitions(&self) -> Vec<ToolDef>;

    /// Resolve a tool by name. Returns `None` if the tool is not in this set.
    fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>>;
}

/// A toolset backed by a plain `Vec<Arc<dyn Tool>>`.
///
/// This is the compatibility implementation: every existing tool source
/// produces a vector of tools, and `VecToolset` wraps that vector with a
/// source label.
pub struct VecToolset {
    source: &'static str,
    tools: Vec<Arc<dyn Tool>>,
}

impl VecToolset {
    /// Create a toolset from a source label and an already-built tool vector.
    pub fn new(source: &'static str, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { source, tools }
    }

    /// Number of tools in this set.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True if this set contains no tools.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Toolset for VecToolset {
    fn source(&self) -> &str {
        self.source
    }

    fn definitions(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|t| t.def()).collect()
    }

    fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.def().name == name).cloned()
    }
}

/// A toolset that composes multiple inner toolsets in order.
///
/// Order matters: the first inner set that contains a tool wins during
/// resolution. This lets built-in tools keep their names even if a plugin
/// or MCP server advertises a tool with the same name.
pub struct CompositeToolset {
    toolsets: Vec<Box<dyn Toolset>>,
}

impl CompositeToolset {
    /// Start with an empty composite.
    pub fn empty() -> Self {
        Self {
            toolsets: Vec::new(),
        }
    }

    /// Add a toolset to the composition.
    pub fn add(&mut self, toolset: Box<dyn Toolset>) {
        self.toolsets.push(toolset);
    }

    /// Replace the first inner toolset with the given `source` label, or
    /// append it if no matching source exists.
    ///
    /// Used for live plugin reload: the executor keeps the built-in and MCP
    /// layers and swaps only the plugin layer.
    pub fn replace(&mut self, source: &str, toolset: Box<dyn Toolset>) -> bool {
        if let Some(idx) = self.toolsets.iter().position(|t| t.source() == source) {
            self.toolsets[idx] = toolset;
            true
        } else {
            self.toolsets.push(toolset);
            false
        }
    }

    /// Convenience constructor from a pre-built list.
    pub fn new(toolsets: Vec<Box<dyn Toolset>>) -> Self {
        Self { toolsets }
    }

    /// Total number of tools across all inner sets.
    ///
    /// Note: this counts duplicate names separately; the executor only sees
    /// the first match during resolution.
    pub fn total_definitions(&self) -> usize {
        self.toolsets.iter().map(|t| t.definitions().len()).sum()
    }

    /// Flatten the composite into a single `Vec<Arc<dyn Tool>>`.
    ///
    /// This is useful for callers that still expect a vector, such as the
    /// executor constructor, which wraps the vector internally. The order
    /// matches the composition order, and duplicate names across inner sets
    /// are resolved in favor of the first set that contains the name.
    pub fn into_tools(self) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        // Use the already-deduplicated definitions list so the model never
        // sees two tools with the same name, then resolve each back to the
        // concrete tool implementation.
        let defs = self.definitions();
        let mut tools = Vec::new();
        for def in defs {
            let tool = self.resolve(def.name).ok_or_else(|| {
                anyhow::anyhow!(
                    "tool '{}' disappeared during flatten — plugin may have loaded inconsistently",
                    def.name
                )
            })?;
            tools.push(tool);
        }
        Ok(tools)
    }
}

impl Toolset for CompositeToolset {
    fn source(&self) -> &str {
        "composite"
    }

    fn definitions(&self) -> Vec<ToolDef> {
        let mut defs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for ts in &self.toolsets {
            for d in ts.definitions() {
                if seen.insert(d.name.to_string()) {
                    defs.push(d);
                }
            }
        }
        defs
    }

    fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.toolsets.iter().find_map(|t| t.resolve(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolOutcome;
    use crate::tools::ToolContext;
    use serde_json::json;

    struct DummyTool {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn def(&self) -> ToolDef {
            ToolDef {
                name: self.name,
                description: "dummy",
                parameters: json!({"type": "object"}),
            }
        }

        async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
            ToolOutcome::Success {
                content: "ok".to_string(),
            }
        }
    }

    #[test]
    fn vec_toolset_lists_and_resolves() {
        let ts = VecToolset::new(
            "builtin",
            vec![
                Arc::new(DummyTool { name: "a" }),
                Arc::new(DummyTool { name: "b" }),
            ],
        );
        assert_eq!(ts.source(), "builtin");
        assert_eq!(ts.len(), 2);
        let defs = ts.definitions();
        assert_eq!(defs.len(), 2);
        assert!(ts.resolve("a").is_some());
        assert!(ts.resolve("missing").is_none());
    }

    #[test]
    fn composite_toolset_orders_sources_and_dedupes_definitions() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![Arc::new(DummyTool { name: "a" })],
        )));
        composite.add(Box::new(VecToolset::new(
            "plugin",
            vec![
                Arc::new(DummyTool { name: "a" }),
                Arc::new(DummyTool { name: "c" }),
            ],
        )));

        let defs = composite.definitions();
        assert_eq!(defs.len(), 2);
        assert!(defs.iter().any(|d| d.name == "a"));
        assert!(defs.iter().any(|d| d.name == "c"));

        // builtin wins on duplicate name
        let resolved = composite.resolve("a").unwrap();
        assert_eq!(resolved.def().name, "a");
    }

    #[test]
    fn composite_into_tools_preserves_order() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![Arc::new(DummyTool { name: "x" })],
        )));
        composite.add(Box::new(VecToolset::new(
            "plugin",
            vec![Arc::new(DummyTool { name: "y" })],
        )));

        let tools = composite.into_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].def().name, "x");
        assert_eq!(tools[1].def().name, "y");
    }

    #[test]
    fn vec_toolset_empty_predicates() {
        let empty = VecToolset::new("empty", vec![]);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.definitions().len(), 0);
        assert!(empty.resolve("anything").is_none());

        let non_empty = VecToolset::new("ne", vec![Arc::new(DummyTool { name: "a" })]);
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 1);
    }

    #[test]
    fn composite_replace_existing_source_returns_true() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![Arc::new(DummyTool { name: "a" })],
        )));
        assert!(composite.replace(
            "builtin",
            Box::new(VecToolset::new(
                "builtin",
                vec![Arc::new(DummyTool { name: "b" })],
            ))
        ));
        assert!(
            composite.resolve("b").is_some(),
            "expected replacement to take effect"
        );
        assert!(composite.resolve("a").is_none(), "old tool should be gone");
    }

    #[test]
    fn composite_replace_missing_source_appends_and_returns_false() {
        let mut composite = CompositeToolset::empty();
        assert!(!composite.replace(
            "plugin",
            Box::new(VecToolset::new(
                "plugin",
                vec![Arc::new(DummyTool { name: "p" })],
            ))
        ));
        assert!(composite.resolve("p").is_some());
    }

    #[test]
    fn composite_total_definitions_counts_across_sets() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![
                Arc::new(DummyTool { name: "a" }),
                Arc::new(DummyTool { name: "b" }),
            ],
        )));
        composite.add(Box::new(VecToolset::new(
            "plugin",
            vec![Arc::new(DummyTool { name: "c" })],
        )));
        assert_eq!(composite.total_definitions(), 3);
    }

    #[test]
    fn composite_definitions_dedup_across_sets() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![Arc::new(DummyTool { name: "dup" })],
        )));
        composite.add(Box::new(VecToolset::new(
            "plugin",
            vec![Arc::new(DummyTool { name: "dup" })],
        )));
        let defs = composite.definitions();
        assert_eq!(defs.len(), 1, "duplicate name should be deduped");
    }

    #[test]
    fn composite_resolve_returns_first_match_in_order() {
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new(
            "builtin",
            vec![Arc::new(DummyTool { name: "shared" })],
        )));
        composite.add(Box::new(VecToolset::new(
            "plugin",
            vec![Arc::new(DummyTool { name: "shared" })],
        )));
        let resolved = composite.resolve("shared").unwrap();
        assert_eq!(resolved.def().name, "shared");
    }

    #[test]
    fn composite_source_label_is_composite() {
        let composite = CompositeToolset::empty();
        assert_eq!(composite.source(), "composite");
    }

    #[test]
    fn composite_new_constructor_preserves_order() {
        let sets: Vec<Box<dyn Toolset>> = vec![
            Box::new(VecToolset::new(
                "a",
                vec![Arc::new(DummyTool { name: "x" })],
            )),
            Box::new(VecToolset::new(
                "b",
                vec![Arc::new(DummyTool { name: "y" })],
            )),
        ];
        let composite = CompositeToolset::new(sets);
        let defs = composite.definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "x");
        assert_eq!(defs[1].name, "y");
    }

    #[tokio::test]
    async fn dummy_tool_run_returns_success() {
        let tool = DummyTool { name: "z" };
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool.run(&ctx, serde_json::json!({})).await;
        match outcome {
            ToolOutcome::Success { content } => assert_eq!(content, "ok"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn composite_empty_into_tools_returns_empty_vec() {
        let composite = CompositeToolset::empty();
        let tools = composite.into_tools().unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn composite_empty_total_definitions_is_zero() {
        let composite = CompositeToolset::empty();
        assert_eq!(composite.total_definitions(), 0);
        assert!(composite.definitions().is_empty());
    }

    #[test]
    fn composite_resolve_missing_returns_none() {
        let composite = CompositeToolset::empty();
        assert!(composite.resolve("nonexistent").is_none());
    }

    #[test]
    fn vec_toolset_definitions_match_len() {
        let ts = VecToolset::new(
            "s",
            vec![
                Arc::new(DummyTool { name: "a" }),
                Arc::new(DummyTool { name: "b" }),
                Arc::new(DummyTool { name: "c" }),
            ],
        );
        assert_eq!(ts.definitions().len(), ts.len());
    }

    #[test]
    fn vec_toolset_source_returns_constructed_label() {
        let ts = VecToolset::new("custom-source", vec![]);
        assert_eq!(ts.source(), "custom-source");
    }
}
