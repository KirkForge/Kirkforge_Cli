//! Scout subagent — read-only in-process exploration.
//!
//! The scout is a stripped-down sibling of the `/explore` persona
//! (see `src/tui/commands/persona.rs`). Where `/explore` always
//! forks a conversation and runs a separate executor in a
//! background task, the scout runs synchronously in the calling
//! task and never touches the conversation log. The trade-off:
//!
//! - `/explore` — full model turn in a fork, isolated context, async
//!   completion. Best for "go deep" research questions.
//! - scout      — single read-only tool call sequence, no fork, no
//!   conversation pollution. Best for "find this file / line" lookups
//!   where the model doesn't need to be involved.
//!
//! The "stripped down" promise is enforced by [`SCOUT_TOOLS`]: the
//! filter accepts only the read-only tool set, and any tool whose
//! name is not in that set is rejected by
//! [`ScoutSubagent::filter_tools`]. The filter is the source of
//! truth — a test pins it to the canonical set so a future
//! addition can't silently broaden the surface.

use crate::tools::Tool;

/// The canonical set of read-only tool names the scout is allowed
/// to use. The list is a `const` so the test suite can pin it
/// without having to read the runtime toolset first.
///
/// Note: this intentionally omits `bash` (the previous
/// `/explore`-style persona allowed it under plan mode; the scout
/// is more conservative). Read-only `bash` is genuinely useful but
/// it adds a non-trivial attack surface (a path-traversal-bypass
/// in the bash sandbox would expose the whole host) so it stays
/// out until the sandbox is independently audited.
pub const SCOUT_TOOLS: &[&str] = &["read_file", "read_image", "grep", "glob"];

/// Lightweight read-only subagent. Holds the toolset it is
/// permitted to use and provides the filter that produces a
/// read-only subset of a larger toolset.
///
/// The struct is a value type: it owns no I/O resources, no
/// adapters, no channels. Construction is cheap; calling
/// `filter_tools` is pure. That keeps the scout trivially
/// testable and lets callers use it as a `dyn`-free filter
/// without any async machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoutSubagent {
    /// The tool names the scout is allowed to use. Stored as a
    /// `Vec<String>` (not a `&[&str]`) so the scout can be
    /// customised at runtime — a future config flag could narrow
    /// the set further. The default constructor initialises it
    /// from [`SCOUT_TOOLS`].
    pub allowed: Vec<String>,
}

impl Default for ScoutSubagent {
    fn default() -> Self {
        Self::new()
    }
}

impl ScoutSubagent {
    /// Build a scout with the canonical read-only tool set.
    pub fn new() -> Self {
        Self {
            allowed: SCOUT_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Build a scout with a custom allow-list. Used by tests and
    /// by future config-driven narrowing.
    pub fn with_allowed<I, S>(allowed: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: allowed.into_iter().map(Into::into).collect(),
        }
    }

    /// Filter a tool list to the scout's read-only set. Tools
    /// whose name is not in `allowed` are dropped. The output is
    /// the same `Arc<dyn Tool>` pointers the caller passed in —
    /// no cloning, no allocation beyond the returned `Vec`.
    pub fn filter_tools(
        &self,
        tools: Vec<std::sync::Arc<dyn Tool>>,
    ) -> Vec<std::sync::Arc<dyn Tool>> {
        tools
            .into_iter()
            .filter(|t| self.allowed.iter().any(|a| a == t.def().name))
            .collect()
    }

    /// Test helper: list the canonical tool names. Equivalent to
    /// reading [`SCOUT_TOOLS`] but produces owned `String`s so the
    /// caller doesn't have to deal with `&str` lifetimes.
    pub fn allowed_names(&self) -> Vec<String> {
        self.allowed.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{ToolDef, ToolOutcome};
    use crate::tools::{Tool, ToolContext};
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Stub tool with a fixed name. Used by the filter tests to
    /// exercise the allow-list without booting a real tool
    /// registry. The `run` body is unreachable from the filter
    /// tests, so the unimplemented!() there is fine.
    struct StubTool {
        name: &'static str,
    }

    impl StubTool {
        fn new(name: &'static str) -> Self {
            Self { name }
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn def(&self) -> ToolDef {
            ToolDef {
                name: self.name,
                description: "stub tool for scout filter tests",
                parameters: serde_json::json!({}),
            }
        }

        async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
            // Tests that exercise `run` build their own stubs; the
            // filter tests don't reach this code path.
            unimplemented!("stub tool: no real implementation")
        }
    }

    fn stub(name: &'static str) -> Arc<dyn Tool> {
        Arc::new(StubTool::new(name))
    }

    /// The default scout only allows the canonical read-only set.
    #[test]
    fn scout_default_allowed_is_canonical() {
        let s = ScoutSubagent::new();
        let names = s.allowed_names();
        for name in SCOUT_TOOLS {
            assert!(names.iter().any(|n| n == *name), "scout must allow {name}");
        }
        // The default has exactly the canonical set, no extras.
        assert_eq!(names.len(), SCOUT_TOOLS.len());
    }

    /// `SCOUT_TOOLS` is pinned so a future broadening is
    /// intentional, not a regression.
    #[test]
    fn scout_tools_constant_is_pinned() {
        assert_eq!(SCOUT_TOOLS, &["read_file", "read_image", "grep", "glob"]);
    }

    /// `filter_tools` keeps read-only tools and drops everything
    /// else. The filter is the source of truth for the read-only
    /// guarantee.
    #[test]
    fn filter_tools_keeps_only_readonly() {
        let s = ScoutSubagent::new();
        let tools: Vec<Arc<dyn Tool>> = vec![
            stub("read_file"),
            stub("read_image"),
            stub("grep"),
            stub("glob"),
            stub("bash"),
            stub("edit_file"),
            stub("write_file"),
            stub("not_a_real_tool"),
        ];
        let filtered = s.filter_tools(tools);
        let names: Vec<String> = filtered.iter().map(|t| t.def().name.to_string()).collect();
        assert_eq!(names.len(), SCOUT_TOOLS.len());
        // All filtered names must be in the allow-list.
        for n in &names {
            assert!(SCOUT_TOOLS.contains(&n.as_str()), "filter leaked {n}");
        }
        // No mutating tool survived.
        for forbidden in ["bash", "edit_file", "write_file"] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "filter must drop {forbidden}"
            );
        }
    }

    /// A custom allow-list narrows the scout further. The filter
    /// must use the customised set, not the canonical one.
    #[test]
    fn filter_tools_uses_custom_allowed_set() {
        let s = ScoutSubagent::with_allowed(["read_file"]);
        let tools: Vec<Arc<dyn Tool>> = vec![stub("read_file"), stub("grep"), stub("glob")];
        let filtered = s.filter_tools(tools);
        let names: Vec<String> = filtered.iter().map(|t| t.def().name.to_string()).collect();
        assert_eq!(names, vec!["read_file".to_string()]);
    }

    /// Filtering an empty tool list yields an empty list — no
    /// panic, no fallback to the canonical set.
    #[test]
    fn filter_tools_empty_input_yields_empty() {
        let s = ScoutSubagent::new();
        let filtered = s.filter_tools(vec![]);
        assert!(filtered.is_empty());
    }

    /// A tool whose name is in the allow-list but whose `def()`
    /// is unique-typed still survives — the filter compares
    /// names, not `dyn`-Trait identity.
    #[test]
    fn filter_tools_preserves_arc_identity() {
        let s = ScoutSubagent::new();
        let keep = stub("read_file");
        let drop = stub("bash");
        let keep_ptr = Arc::clone(&keep) as Arc<dyn Tool>;
        let tools: Vec<Arc<dyn Tool>> = vec![keep_ptr, drop];
        let filtered = s.filter_tools(tools);
        assert_eq!(filtered.len(), 1);
        // The kept tool's pointer is the same Arc we passed in
        // (no clone, no wrapper).
        assert!(Arc::ptr_eq(&filtered[0], &keep));
    }

    /// Sanity check: the stub factory produces a tool whose
    /// `def().name` matches the requested name. Catches a future
    /// refactor that accidentally breaks the `name: self.name`
    /// path in `StubTool::def`.
    #[test]
    fn stub_tool_compiles_with_no_extra_imports() {
        let s = stub("read_file");
        assert_eq!(s.def().name, "read_file");
    }
}
