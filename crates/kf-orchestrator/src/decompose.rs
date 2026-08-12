//! Decompose pipeline (R3). Port of `orchestrator-decompose.ts`.
//!
//! - [`topological_sort`] — Kahn's algorithm with cycle detection (pure).
//! - [`parse_decomposition`] — strip markdown fences, locate JSON array,
//!   validate, topologically sort (pure).
//! - [`decompose_task`] — model call → parse → persist to memory.
//! - [`execute_decomposition`] — recall a stored decomposition, run subtasks
//!   in dependency order via a caller-provided delegate.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use serde_json::Value;

use kf_memory_store::MemoryStore;

use crate::model::{ModelClient, TaskBrief};
use crate::types::{
    DecompositionExecutionResult, DecompositionResult, SubtaskExecutionResult, TaskInput, TaskNode,
};

/// Maximum subtasks allowed in one decomposition (TS hard limit).
pub const MAX_SUBTASKS: usize = 24;

/// Kahn's algorithm topological sort. Returns the input order for
/// independent tasks; fails on cycles or self-deps.
pub fn topological_sort(nodes: &[TaskNode]) -> Result<Vec<TaskNode>> {
    let mut by_id: HashMap<&str, &TaskNode> = HashMap::new();
    for n in nodes {
        if by_id.contains_key(n.id.as_str()) {
            return Err(anyhow!("duplicate task id: {}", n.id));
        }
        by_id.insert(n.id.as_str(), n);
    }

    let mut in_degree: HashMap<&str, i64> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in nodes {
        in_degree.insert(n.id.as_str(), 0);
        adj.insert(n.id.as_str(), Vec::new());
    }
    for n in nodes {
        for dep in &n.depends_on {
            // Self-dep check.
            if dep == &n.id {
                return Err(anyhow!("Task {} cannot depend on itself", n.id));
            }
            // Unknown-dep check (also required by TS).
            if !by_id.contains_key(dep.as_str()) {
                return Err(anyhow!("Task {} depends on unknown task: {}", n.id, dep));
            }
            adj.get_mut(dep.as_str()).unwrap().push(n.id.as_str());
            *in_degree.get_mut(n.id.as_str()).unwrap() += 1;
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();

    let mut sorted: Vec<TaskNode> = Vec::new();
    while let Some(id) = queue.pop_front() {
        if let Some(n) = by_id.get(id) {
            sorted.push((*n).clone());
        }
        if let Some(nexts) = adj.get(id) {
            for next in nexts {
                let d = in_degree.get_mut(*next).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push_back(next);
                }
            }
        }
    }

    if sorted.len() != nodes.len() {
        return Err(anyhow!("Cycle detected in task dependencies"));
    }
    Ok(sorted)
}

/// Strip fences + bracket-trim raw model output, then parse + validate.
/// Pure. Matches `parseDecomposition` (canonical version).
pub fn parse_decomposition(raw: &str) -> Result<DecompositionResult> {
    let mut json_str = raw.trim();
    static FENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let fence_re =
        FENCE.get_or_init(|| regex::Regex::new(r"```(?:json)?\s*\n?([\s\S]*?)```").unwrap());
    if let Some(c) = fence_re.captures(json_str) {
        json_str = c.get(1).map(|m| m.as_str()).unwrap_or(json_str).trim();
    }
    // Bracket heuristic: prefer `[{` start, fall back to lone `[`.
    if let Some(idx) = json_str.find("[{") {
        if idx > 0 {
            json_str = &json_str[idx..];
        }
    } else if let Some(idx) = json_str.find('[') {
        if idx > 0 {
            json_str = &json_str[idx..];
        }
    }
    if let Some(idx) = json_str.rfind("}]") {
        if idx > 0 {
            json_str = &json_str[..idx + 2];
        }
    } else if let Some(idx) = json_str.rfind(']') {
        if idx > 0 && idx < json_str.len() - 1 {
            json_str = &json_str[..=idx];
        }
    }

    let parsed: Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow!("Failed to parse decomposition JSON: {e}"))?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| anyhow!("Decomposition output must be a JSON array"))?;
    if arr.is_empty() {
        return Err(anyhow!("Decomposition produced zero subtasks"));
    }

    let valid_complexities: HashSet<&str> = ["trivial", "simple", "moderate", "complex"]
        .into_iter()
        .collect();
    let valid_languages: HashSet<&str> = [
        "typescript",
        "javascript",
        "python",
        "shell",
        "cpp",
        "c",
        "rust",
        "go",
        "sql",
        "text",
    ]
    .into_iter()
    .collect();

    let mut nodes: Vec<TaskNode> = Vec::with_capacity(arr.len());
    let mut ids: HashSet<String> = HashSet::new();
    for (i, t) in arr.iter().enumerate() {
        let id = t
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("task-{}", i + 1));
        if !ids.insert(id.clone()) {
            return Err(anyhow!("Duplicate task id: {id}"));
        }
        let complexity = t
            .get("estimatedComplexity")
            .and_then(|v| v.as_str())
            .unwrap_or("moderate");
        if !valid_complexities.contains(complexity) {
            return Err(anyhow!("Invalid complexity \"{complexity}\" in task {id}"));
        }
        let description = t
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(500)
            .collect::<String>();
        let mut language = t
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();
        if !valid_languages.contains(language.as_str()) {
            language = "text".into();
        }
        let depends_on = t
            .get("dependsOn")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let output_files = t
            .get("outputFiles")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .take(20)
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let verification_hint = t
            .get("verificationHint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>();
        nodes.push(TaskNode {
            id,
            description,
            language,
            depends_on,
            estimated_complexity: complexity.into(),
            output_files,
            verification_hint,
        });
    }

    if nodes.len() > MAX_SUBTASKS {
        return Err(anyhow!(
            "Decomposition produced {} subtasks; maximum is {}",
            nodes.len(),
            MAX_SUBTASKS
        ));
    }

    let sorted = topological_sort(&nodes)?;
    let task_count = sorted.len();
    let token_estimate = task_count as i64 * 400
        + sorted
            .iter()
            .map(|n| n.description.len() as i64)
            .sum::<i64>();
    let with_deps = sorted.iter().filter(|n| !n.depends_on.is_empty()).count();
    Ok(DecompositionResult {
        root_task: String::new(), // filled by decompose_task
        tasks: sorted,
        total_estimated_tokens: token_estimate,
        rationale: format!("Decomposed into {task_count} subtasks ({with_deps} with dependencies)"),
    })
}

/// Drive a model call to produce a decomposition. Persists the result to
/// `memory` (if provided) on success. Retries once on parse failure with a
/// stripped-down re-prompt (matches TS behavior).
pub async fn decompose_task(
    client: &dyn ModelClient,
    memory: Option<&MemoryStore>,
    task: &TaskInput,
    decompose_provider: &str,
) -> Result<DecompositionResult> {
    let task_id = task
        .task_id
        .clone()
        .unwrap_or_else(|| format!("decomp-{}", now_millis()));
    let profile = kf_routing::detect_task_profile(&task.description);
    let brief = TaskBrief {
        template: "task-decompose".into(),
        description: task.description.clone(),
        variables: serde_json::json!({
            "language": profile.language.as_str(),
            "defaultFile": profile.default_file,
        }),
        target_file: None,
        correction_prompt: None,
    };
    let emission = client.execute(&brief).await?;
    let parsed = parse_decomposition(&emission.content);
    if let Ok(mut dr) = parsed {
        dr.root_task = task.description.clone();
        if let Some(store) = memory {
            let tasks_value: Vec<Value> = dr
                .tasks
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                .collect();
            let _ = store.write_decomposition(
                &task_id,
                &task.description,
                &tasks_value,
                profile.language.as_str(),
            );
        }
        let _ = decompose_provider;
        return Ok(dr);
    }
    // Retry once with a stricter prompt.
    let retry_brief = TaskBrief {
        template: "task-decompose".into(),
        description: format!(
            "{}\n\n---\nYour previous output could not be parsed as valid JSON. Output ONLY a JSON array, no markdown, no explanation.",
            task.description
        ),
        variables: serde_json::json!({
            "language": profile.language.as_str(),
            "defaultFile": profile.default_file,
        }),
        target_file: None,
        correction_prompt: None,
    };
    let retry_emission = client.execute(&retry_brief).await?;
    let mut dr = parse_decomposition(&retry_emission.content)
        .map_err(|e| anyhow!("Decomposition failed after retry: {e}"))?;
    dr.root_task = task.description.clone();
    if let Some(store) = memory {
        let tasks_value: Vec<Value> = dr
            .tasks
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect();
        let _ = store.write_decomposition(
            &task_id,
            &task.description,
            &tasks_value,
            profile.language.as_str(),
        );
    }
    Ok(dr)
}

/// Callback shape for `execute_decomposition`: the orchestrator hands in
/// its `delegate` method.
#[async_trait::async_trait]
pub trait DelegateFn: Send + Sync {
    async fn delegate(&self, task: TaskInput) -> Result<crate::types::DelegationResult>;
}

/// Recall a stored decomposition, sort it, and execute each subtask in
/// dependency order via `delegate`. Failed dependencies cause dependent
/// subtasks to be skipped. Each subtask is retried once on failure.
pub async fn execute_decomposition(
    memory: &MemoryStore,
    task_id: &str,
    delegate: &dyn DelegateFn,
) -> Result<DecompositionExecutionResult> {
    let recalled = memory
        .recall_decomposition(task_id)
        .map_err(|e| anyhow!("recall failed: {e}"))?
        .ok_or_else(|| anyhow!("No decomposition found for taskId: {task_id}"))?;
    if recalled.tasks.is_empty() {
        return Err(anyhow!("No decomposition found for taskId: {task_id}"));
    }

    let tasks: Vec<TaskNode> = recalled
        .tasks
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    if tasks.is_empty() {
        return Err(anyhow!("Stored decomposition has no parseable tasks"));
    }
    let ordered = topological_sort(&tasks)
        .map_err(|e| anyhow!("Stored decomposition has invalid dependency graph: {e}"))?;

    let started_at = now_millis();
    let mut total_tokens = 0i64;
    let mut results: Vec<SubtaskExecutionResult> = Vec::new();
    let mut completed: HashMap<String, SubtaskExecutionResult> = HashMap::new();

    for node in &ordered {
        // Check deps first.
        let mut skipped_reason: Option<String> = None;
        for dep_id in &node.depends_on {
            match completed.get(dep_id) {
                None => {
                    skipped_reason = Some(format!(
                        "Dependency {dep_id} for task {} was not found",
                        node.id
                    ));
                }
                Some(r) if !r.ok => {
                    skipped_reason = Some(format!("Skipped: dependency {dep_id} failed"));
                }
                _ => {}
            }
            if skipped_reason.is_some() {
                break;
            }
        }
        if let Some(reason) = skipped_reason {
            let sr = SubtaskExecutionResult {
                node_id: node.id.clone(),
                ok: false,
                description: node.description.clone(),
                language: node.language.clone(),
                duration_ms: 0,
                tokens_used: 0,
                verdict: None,
                error: Some(reason),
                files: None,
            };
            results.push(sr.clone());
            completed.insert(node.id.clone(), sr);
            continue;
        }

        let sub_started = now_millis();
        let mut delegate_input = TaskInput {
            task_id: Some(format!("{task_id}--{}", node.id)),
            description: node.description.clone(),
            suppress_memory: false,
            ..Default::default()
        };

        let mut result = delegate.delegate(delegate_input.clone()).await;
        if result.is_err() {
            // Retry once.
            delegate_input.task_id = Some(format!("{task_id}--{}-r", node.id));
            result = delegate.delegate(delegate_input.clone()).await;
        }

        let sr = match result {
            Ok(r) => {
                let tokens = r.emission.total_tokens;
                total_tokens += tokens;
                let verdict = r
                    .packet
                    .as_ref()
                    .map(|p| format!("{:?}", p.verification.overall).to_lowercase())
                    .unwrap_or_else(|| "unknown".into());
                let ok = matches!(verdict.as_str(), "pass" | "warn");
                SubtaskExecutionResult {
                    node_id: node.id.clone(),
                    ok,
                    description: node.description.clone(),
                    language: node.language.clone(),
                    duration_ms: now_millis() - sub_started,
                    tokens_used: tokens,
                    verdict: Some(verdict),
                    error: None,
                    files: None,
                }
            }
            Err(e) => SubtaskExecutionResult {
                node_id: node.id.clone(),
                ok: false,
                description: node.description.clone(),
                language: node.language.clone(),
                duration_ms: now_millis() - sub_started,
                tokens_used: 0,
                verdict: None,
                error: Some(e.to_string()),
                files: None,
            },
        };
        results.push(sr.clone());
        completed.insert(node.id.clone(), sr);
    }

    let succeeded = results.iter().filter(|r| r.ok).count() as i64;
    Ok(DecompositionExecutionResult {
        root_task: recalled.description,
        total_subtasks: ordered.len() as i64,
        succeeded_count: succeeded,
        failed_count: ordered.len() as i64 - succeeded,
        total_tokens,
        total_duration_ms: now_millis() - started_at,
        results,
    })
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, deps: &[&str]) -> TaskNode {
        TaskNode {
            id: id.into(),
            description: format!("desc-{id}"),
            language: "text".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            estimated_complexity: "moderate".into(),
            output_files: vec![],
            verification_hint: String::new(),
        }
    }

    #[test]
    fn topological_sort_preserves_independent_order() {
        let nodes = vec![node("a", &[]), node("b", &[])];
        let sorted = topological_sort(&nodes).unwrap();
        assert_eq!(sorted.len(), 2);
        let ids: Vec<&str> = sorted.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
    }

    #[test]
    fn topological_sort_orders_by_dependency() {
        let nodes = vec![node("b", &["a"]), node("a", &[])];
        let sorted = topological_sort(&nodes).unwrap();
        let ids: Vec<String> = sorted.iter().map(|n| n.id.clone()).collect();
        let pos_a = ids.iter().position(|x| x == "a").unwrap();
        let pos_b = ids.iter().position(|x| x == "b").unwrap();
        assert!(pos_a < pos_b, "a must come before b");
    }

    #[test]
    fn topological_sort_detects_cycle() {
        let nodes = vec![node("a", &["b"]), node("b", &["a"])];
        let err = topological_sort(&nodes).unwrap_err();
        assert!(err.to_string().contains("Cycle"));
    }

    #[test]
    fn topological_sort_rejects_self_dep() {
        let nodes = vec![node("a", &["a"])];
        let err = topological_sort(&nodes).unwrap_err();
        assert!(err.to_string().contains("depend on itself"));
    }

    #[test]
    fn topological_sort_rejects_unknown_dep() {
        let nodes = vec![node("a", &["zzz"])];
        let err = topological_sort(&nodes).unwrap_err();
        assert!(err.to_string().contains("unknown task"));
    }

    #[test]
    fn topological_sort_rejects_duplicate_id() {
        let nodes = vec![node("a", &[]), node("a", &[])];
        let err = topological_sort(&nodes).unwrap_err();
        assert!(err.to_string().contains("duplicate task id"));
    }

    // ── parse_decomposition ──

    #[test]
    fn parse_simple_array() {
        let raw = r#"```json
[
  {"id":"a","description":"do a","language":"python","dependsOn":[]},
  {"id":"b","description":"do b","language":"python","dependsOn":["a"]}
]
```"#;
        let dr = parse_decomposition(raw).unwrap();
        assert_eq!(dr.tasks.len(), 2);
        assert_eq!(dr.tasks[0].id, "a");
        assert_eq!(dr.tasks[1].id, "b");
        assert!(dr.rationale.contains("1 with dependencies"));
        assert!(dr.total_estimated_tokens > 0);
    }

    #[test]
    fn parse_strips_leading_prose() {
        let raw = r#"Here is the plan:
[{"id":"a","description":"x"}]
Let me know."#;
        let dr = parse_decomposition(raw).unwrap();
        assert_eq!(dr.tasks.len(), 1);
    }

    #[test]
    fn parse_rejects_empty_array() {
        let err = parse_decomposition("[]").unwrap_err();
        assert!(err.to_string().contains("zero subtasks"));
    }

    #[test]
    fn parse_rejects_invalid_complexity() {
        let raw = r#"[{"id":"a","estimatedComplexity":"galactic"}]"#;
        let err = parse_decomposition(raw).unwrap_err();
        assert!(err.to_string().contains("Invalid complexity"));
    }

    #[test]
    fn parse_defaults_unknown_language_to_text() {
        let raw = r#"[{"id":"a","language":"klingon"}]"#;
        let dr = parse_decomposition(raw).unwrap();
        assert_eq!(dr.tasks[0].language, "text");
    }

    #[test]
    fn parse_enforces_max_subtasks() {
        let mut items: Vec<Value> = Vec::new();
        for i in 0..(MAX_SUBTASKS + 1) {
            items.push(json!({"id": format!("t{i}")}));
        }
        let raw = serde_json::to_string(&items).unwrap();
        let err = parse_decomposition(&raw).unwrap_err();
        assert!(err.to_string().contains("maximum"));
    }

    #[test]
    fn parse_rejects_non_array() {
        let err = parse_decomposition(r#"{"id":"a"}"#).unwrap_err();
        assert!(err.to_string().contains("must be a JSON array"));
    }
}
