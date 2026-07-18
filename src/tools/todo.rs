//! Session-scoped TODO list — a user-facing progress panel the model
//! writes to as it works. Ports vix's `session_todo.go` validation design
//! (not the Go code): replace semantics with stable ids, `depends_on`,
//! and server-side rejection of duplicate ids, self-deps, dangling refs,
//! dependency cycles, and `in_progress` items whose dependencies are not
//! yet `completed`.
//!
//! State is an `Arc<Mutex<Vec<TodoItem>>>` shared between `TodoWrite` and
//! `TodoRead` so a read always reflects the last write. One `Arc` is
//! constructed per toolset (per session) in `all_tools`; both tools get a
//! clone. No new dependencies; no network; no filesystem.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// One TODO entry. `id` is chosen by the model and must be stable across
/// updates so later writes can refer back to the same item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    /// Ids of items that must be `Completed` before this one may become
    /// `InProgress`. Optional; defaults to empty.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Lifecycle of a TODO item. Serialized as the lowercase string so the
/// JSON schema's `enum: ["pending","in_progress","completed"]` round-trips
/// without a rename.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[>]",
            Self::Completed => "[x]",
        }
    }
}

/// Shared, session-scoped TODO list.
pub type TodoState = Arc<Mutex<Vec<TodoItem>>>;

pub struct TodoWrite {
    state: TodoState,
}

impl TodoWrite {
    pub fn new(state: TodoState) -> Self {
        Self { state }
    }
}

pub struct TodoRead {
    state: TodoState,
}

impl TodoRead {
    pub fn new(state: TodoState) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl Tool for TodoWrite {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "todo_write",
            description: "Replace the session TODO list with the provided items. This list is \
USER-FACING: it renders live in the UI as a progress panel, so it is how the user follows what \
you are doing. Keep it current — call every time you add an item, change a status (mark one \
in_progress before starting, completed immediately after finishing), or remove finished work. \
Replace semantics: the list you send fully overwrites the previous one. Keep id values stable \
across updates. Each item has an optional depends_on listing other ids that must be completed \
before this item may become in_progress. Server-side validation rejects: duplicate ids, \
self-dependencies, dangling references, dependency cycles, and any in_progress item whose \
dependencies are not yet completed. Send an empty array to clear the list.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "Full replacement list of TODO items. Send an empty array to clear.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id":         { "type": "string", "description": "Stable id chosen by you. Reuse across updates." },
                                "content":    { "type": "string", "description": "Short description of the step." },
                                "status":     { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                                "depends_on": {
                                    "type": "array",
                                    "description": "Optional ids of items that must be completed before this one can become in_progress.",
                                    "items": { "type": "string" }
                                }
                            },
                            "required": ["id", "content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let items = match parse_todos(&args) {
            Ok(items) => items,
            Err(msg) => return ToolOutcome::Failure(ToolError::invalid_args(msg)),
        };

        if let Err(msg) = validate_todo_list(&items) {
            return ToolOutcome::Failure(ToolError::invalid_args(msg));
        }

        let snapshot = items.clone();
        match self.state.lock() {
            Ok(mut guard) => *guard = items,
            Err(poisoned) => *poisoned.into_inner() = items,
        }

        ToolOutcome::Success {
            content: format!("TODO list updated.\n{}", format_todo_list(&snapshot)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for TodoRead {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "todo_read",
            description: "Return the session's current TODO list. Use to recover authoritative \
state when prior turns may have been compacted out of context, or to check what is pending, in \
progress, completed, or blocked by an unmet dependency.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let snapshot = match self.state.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        ToolOutcome::Success {
            content: format_todo_list(&snapshot),
        }
    }
}

/// Decode the `todos` array from the tool args. Accepts either a JSON
/// array or a JSON-encoded string (models occasionally send the array as
/// a string), matching vix's round-trip-tolerant decode.
fn parse_todos(args: &serde_json::Value) -> Result<Vec<TodoItem>, String> {
    let raw = args
        .get("todos")
        .ok_or_else(|| "todo_write requires a 'todos' array (use [] to clear)".to_string())?;

    let value: serde_json::Value = if let serde_json::Value::String(s) = raw {
        serde_json::from_str(s).map_err(|e| format!("failed to parse todos string as JSON: {e}"))?
    } else {
        raw.clone()
    };

    let arr = value
        .as_array()
        .ok_or_else(|| "'todos' must be an array".to_string())?;

    let mut items = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("item at index {i} missing string 'id'"))?
            .to_string();
        let content = entry
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("item {id:?} missing string 'content'"))?
            .to_string();
        let status_str = entry
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("item {id:?} missing string 'status'"))?;
        let status = TodoStatus::from_str(status_str).ok_or_else(|| {
            format!(
                "item {id:?} has invalid status {status_str:?} (must be pending, in_progress, or completed)"
            )
        })?;
        let depends_on = entry
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.as_str().map(String::from))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        items.push(TodoItem {
            id,
            content,
            status,
            depends_on,
        });
    }
    Ok(items)
}

/// Validate the full replacement list. Returns the first violation as a
/// human-readable message, or `Ok(())`. Order of checks matches vix:
/// shape (empty id/content, bad status, duplicate id) → dependency graph
/// (self-dep, dangling) → cycle → in_progress-with-unmet-dep.
fn validate_todo_list(items: &[TodoItem]) -> Result<(), String> {
    let mut ids: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if it.id.trim().is_empty() {
            return Err(format!("item at index {i} has empty id"));
        }
        if it.content.trim().is_empty() {
            return Err(format!("item {:?} has empty content", it.id));
        }
        if ids.insert(it.id.as_str(), i).is_some() {
            return Err(format!("duplicate id {:?}", it.id));
        }
    }

    for it in items {
        for dep in &it.depends_on {
            if dep == &it.id {
                return Err(format!("item {:?} lists itself in depends_on", it.id));
            }
            if !ids.contains_key(dep.as_str()) {
                return Err(format!("item {:?} depends on unknown id {:?}", it.id, dep));
            }
        }
    }

    if let Some(cycle) = find_cycle(items, &ids) {
        return Err(format!("dependency cycle detected: {cycle}"));
    }

    for it in items {
        if it.status != TodoStatus::InProgress {
            continue;
        }
        let blockers: Vec<&str> = it
            .depends_on
            .iter()
            .filter(|dep| {
                let dep_item = &items[*ids.get(dep.as_str()).unwrap()];
                dep_item.status != TodoStatus::Completed
            })
            .map(String::as_str)
            .collect();
        if !blockers.is_empty() {
            return Err(format!(
                "item {:?} is in_progress but depends on unfinished items: {}",
                it.id,
                blockers.join(", ")
            ));
        }
    }

    Ok(())
}

/// DFS cycle detection over the `depends_on` edges, returning the cycle
/// as an `a -> b -> ... -> a` string for the error message. Colors:
/// white (unvisited), gray (on the current stack), black (fully done).
/// Iteration order is by index so the reported cycle is deterministic.
fn find_cycle(items: &[TodoItem], idx: &std::collections::HashMap<&str, usize>) -> Option<String> {
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;

    let n = items.len();
    let mut color = vec![WHITE; n];
    let mut parent: Vec<isize> = vec![-1; n];
    let mut cycle: Vec<usize> = Vec::new();

    // Recursive DFS with an explicit stack to avoid borrow entanglement.
    // Each frame records (node, next-edge-index). We push children as we
    // descend and pop when a subtree is fully black.
    fn dfs(
        start: usize,
        items: &[TodoItem],
        idx: &std::collections::HashMap<&str, usize>,
        color: &mut [u8],
        parent: &mut [isize],
        cycle: &mut Vec<usize>,
    ) -> bool {
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = GRAY;
        while let Some(&(u, edge_i)) = stack.last() {
            let deps = &items[u].depends_on;
            if edge_i < deps.len() {
                let dep = &deps[edge_i];
                // Advance the frame's edge cursor.
                stack.last_mut().unwrap().1 = edge_i + 1;
                let v = *idx.get(dep.as_str()).unwrap();
                let c = color[v];
                if c == WHITE {
                    parent[v] = u as isize;
                    color[v] = GRAY;
                    stack.push((v, 0));
                } else if c == GRAY {
                    // Back edge → cycle. Walk parents from u back to v.
                    cycle.push(v);
                    let mut x = u as isize;
                    while x != -1 && x != v as isize {
                        cycle.push(x as usize);
                        x = parent[x as usize];
                    }
                    cycle.push(v);
                    return true;
                }
                // BLACK: already fully explored; skip.
            } else {
                color[u] = BLACK;
                stack.pop();
            }
        }
        false
    }

    for i in 0..n {
        if color[i] == WHITE && dfs(i, items, idx, &mut color, &mut parent, &mut cycle) {
            // cycle was built root-first; reverse to get the path order,
            // then join as "a -> b -> ... -> a".
            cycle.reverse();
            let names: Vec<&str> = cycle.iter().map(|&n| items[n].id.as_str()).collect();
            return Some(names.join(" -> "));
        }
    }
    None
}

/// Render the list for the model and the user. Mirrors vix's `formatTodoList`:
/// marker, 1-based index, content, optional `(depends on: ...)`, and a
/// `blocked` tag when an uncompleted dependency is still pending.
fn format_todo_list(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "TODO list is empty.".to_string();
    }

    let completed: std::collections::HashSet<&str> = items
        .iter()
        .filter(|it| it.status == TodoStatus::Completed)
        .map(|it| it.id.as_str())
        .collect();

    let mut out = format!(
        "TODO list ({} item{}):\n",
        items.len(),
        if items.len() == 1 { "" } else { "s" }
    );
    for (i, it) in items.iter().enumerate() {
        let marker = it.status.marker();
        out.push_str(&format!("  {marker} {}. {}", i + 1, it.content));
        if !it.depends_on.is_empty() {
            out.push_str(&format!("  (depends on: {})", it.depends_on.join(", ")));
            if it.status != TodoStatus::Completed {
                let blocked = it
                    .depends_on
                    .iter()
                    .any(|d| !completed.contains(d.as_str()));
                if blocked {
                    out.push_str(" [blocked]");
                }
            }
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> TodoState {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn item(id: &str, content: &str, status: &str) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: content.to_string(),
            status: TodoStatus::from_str(status).unwrap(),
            depends_on: Vec::new(),
        }
    }

    fn write_args(items: &[TodoItem]) -> serde_json::Value {
        serde_json::json!({ "todos": items })
    }

    async fn run_write(tool: &TodoWrite, items: &[TodoItem]) -> ToolOutcome {
        tool.run(&ToolContext::default(), write_args(items)).await
    }

    async fn run_read(tool: &TodoRead) -> ToolOutcome {
        tool.run(&ToolContext::default(), serde_json::json!({}))
            .await
    }

    #[tokio::test]
    async fn rejects_empty_id() {
        let t = TodoWrite::new(state());
        let mut it = item("a", "do x", "pending");
        it.id = "  ".to_string();
        match run_write(&t, &[it]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("empty id")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let t = TodoWrite::new(state());
        let mut it = item("a", "do x", "pending");
        it.content = "".to_string();
        match run_write(&t, &[it]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("empty content")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_invalid_status() {
        let t = TodoWrite::new(state());
        let args = serde_json::json!({
            "todos": [{ "id": "a", "content": "x", "status": "done" }]
        });
        let out = t.run(&ToolContext::default(), args).await;
        match out {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("invalid status")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_duplicate_id() {
        let t = TodoWrite::new(state());
        let out = run_write(&t, &[item("a", "x", "pending"), item("a", "y", "pending")]).await;
        match out {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("duplicate id")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_self_dependency() {
        let t = TodoWrite::new(state());
        let mut it = item("a", "x", "pending");
        it.depends_on = vec!["a".to_string()];
        match run_write(&t, &[it]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("itself")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_dangling_dependency() {
        let t = TodoWrite::new(state());
        let mut it = item("a", "x", "pending");
        it.depends_on = vec!["ghost".to_string()];
        match run_write(&t, &[it]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("unknown id")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_dependency_cycle() {
        let t = TodoWrite::new(state());
        let mut a = item("a", "x", "pending");
        a.depends_on = vec!["b".to_string()];
        let mut b = item("b", "y", "pending");
        b.depends_on = vec!["a".to_string()];
        match run_write(&t, &[a, b]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("cycle")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_in_progress_with_unmet_dependency() {
        let t = TodoWrite::new(state());
        let a = item("a", "blocker", "pending");
        let mut b = item("b", "blocked", "in_progress");
        b.depends_on = vec!["a".to_string()];
        match run_write(&t, &[a, b]).await {
            ToolOutcome::Failure(e) => assert!(e.to_user_message().contains("unfinished")),
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allows_in_progress_when_dependency_completed() {
        let t = TodoWrite::new(state());
        let a = item("a", "blocker", "completed");
        let mut b = item("b", "after", "in_progress");
        b.depends_on = vec!["a".to_string()];
        match run_write(&t, &[a, b]).await {
            ToolOutcome::Success { content } => assert!(content.contains("[>]")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replace_semantics_overwrite_previous_list() {
        let s = state();
        let w = TodoWrite::new(s.clone());
        let r = TodoRead::new(s);

        run_write(
            &w,
            &[
                item("a", "first", "pending"),
                item("b", "second", "pending"),
            ],
        )
        .await;
        let out = run_read(&r).await;
        match out {
            ToolOutcome::Success { content } => assert!(
                content.contains("2 items")
                    && content.contains("first")
                    && content.contains("second")
            ),
            other => panic!("expected Success, got {other:?}"),
        }

        // Replace with a single-item list — "second" must disappear.
        run_write(&w, &[item("only", "third", "pending")]).await;
        let out = run_read(&r).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("third"));
                assert!(!content.contains("first"));
                assert!(!content.contains("second"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_array_clears_the_list() {
        let s = state();
        let w = TodoWrite::new(s.clone());
        let r = TodoRead::new(s);
        run_write(&w, &[item("a", "x", "pending")]).await;
        run_write(&w, &[]).await;
        let out = run_read(&r).await;
        match out {
            ToolOutcome::Success { content } => assert_eq!(content, "TODO list is empty."),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn accepts_todos_as_json_string() {
        let t = TodoWrite::new(state());
        let args = serde_json::json!({
            "todos": r#"[{"id":"a","content":"x","status":"pending"}]"#
        });
        let out = t.run(&ToolContext::default(), args).await;
        match out {
            ToolOutcome::Success { content } => assert!(content.contains("1 item")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn blocked_tag_shown_for_unmet_dependency() {
        let items = vec![item("a", "blocker", "pending"), {
            let mut b = item("b", "after", "pending");
            b.depends_on = vec!["a".to_string()];
            b
        }];
        let rendered = format_todo_list(&items);
        assert!(rendered.contains("(depends on: a) [blocked]"));
    }

    #[test]
    fn validate_cycle_three_way() {
        // a -> b -> c -> a
        let mut a = item("a", "a", "pending");
        a.depends_on = vec!["b".to_string()];
        let mut b = item("b", "b", "pending");
        b.depends_on = vec!["c".to_string()];
        let mut c = item("c", "c", "pending");
        c.depends_on = vec!["a".to_string()];
        let items = vec![a, b, c];
        let mut idx = std::collections::HashMap::new();
        for (i, it) in items.iter().enumerate() {
            idx.insert(it.id.as_str(), i);
        }
        let cycle = find_cycle(&items, &idx).expect("3-way cycle must be detected");
        assert!(cycle.contains("a") && cycle.contains("b") && cycle.contains("c"));
    }
}
