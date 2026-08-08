//! `remember` tool — explicit fact storage for the model.
//!
//! The model calls this to store a fact into the persistent `MemoryStore`.
//! The fact is saved as a markdown file with YAML frontmatter, keyed by a
//! slug derived from the fact text. Duplicate slugs overwrite (idempotent).

use crate::session::memory::{slugify_description, MemoryStore};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};

pub struct Remember {
    store: Option<MemoryStore>,
}

impl Default for Remember {
    fn default() -> Self {
        Self::new()
    }
}

impl Remember {
    pub fn new() -> Self {
        Self { store: None }
    }

    /// Construct with an explicit store (for testing).
    pub fn with_store(store: MemoryStore) -> Self {
        Self { store: Some(store) }
    }

    fn get_store(&self) -> Result<MemoryStore, ToolError> {
        match &self.store {
            Some(s) => Ok(s.clone()),
            None => MemoryStore::default_store()
                .map_err(|e| ToolError::internal(format!("could not open memory store: {e}"))),
        }
    }
}

#[async_trait::async_trait]
impl Tool for Remember {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "remember",
            description: "Store a fact in persistent memory for retrieval in future sessions. \
                Use for user preferences, project conventions, and key decisions. \
                Stored facts are automatically injected into the system prompt when relevant. \
                Calling remember with the same fact content overwrites the previous entry (idempotent).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "fact": {
                        "type": "string",
                        "description": "The fact to remember. Be specific and concise — e.g. 'User prefers tabs over spaces for indentation' rather than 'user has a preference'."
                    },
                    "category": {
                        "type": "string",
                        "description": "Optional category for grouping: 'user', 'project', 'feedback', or 'reference'. Defaults to 'user'."
                    }
                },
                "required": ["fact"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let fact = match args.get("fact").and_then(|v| v.as_str()) {
            Some(f) if !f.trim().is_empty() => f.trim().to_string(),
            _ => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "remember requires a non-empty 'fact' string",
                ));
            }
        };

        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("user");

        let slug = slugify_description(&fact);
        if slug.is_empty() {
            return ToolOutcome::Failure(ToolError::invalid_args(
                "fact text must contain at least one alphanumeric character",
            ));
        }

        let store = match self.get_store() {
            Ok(s) => s,
            Err(e) => return ToolOutcome::Failure(e),
        };

        match store.upsert(&slug, &fact, &fact, category) {
            Ok(_) => ToolOutcome::Success {
                content: format!("Remembered: {fact}"),
            },
            Err(e) => {
                ToolOutcome::Failure(ToolError::internal(format!("could not save fact: {e}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> MemoryStore {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        MemoryStore::open(path).unwrap()
    }

    #[tokio::test]
    async fn remembers_a_fact() {
        let store = temp_store();
        let tool = Remember::with_store(store.clone());
        let args = serde_json::json!({ "fact": "User prefers tabs over spaces" });
        let outcome = tool.run(&ToolContext::new(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("Remembered:"));
                assert!(content.contains("tabs over spaces"));
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let slug = slugify_description("User prefers tabs over spaces");
        let found = store.get(&slug).expect("fact should be stored");
        assert_eq!(found.body, "User prefers tabs over spaces");
        assert_eq!(found.metadata.get("type").map(|s| s.as_str()), Some("user"));
    }

    #[tokio::test]
    async fn rejects_empty_fact() {
        let tool = Remember::with_store(temp_store());
        let args = serde_json::json!({ "fact": "" });
        match tool.run(&ToolContext::new(), args).await {
            ToolOutcome::Failure(e) => {
                assert!(e.to_user_message().contains("non-empty"));
            }
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_whitespace_fact() {
        let tool = Remember::with_store(temp_store());
        let args = serde_json::json!({ "fact": "   " });
        match tool.run(&ToolContext::new(), args).await {
            ToolOutcome::Failure(e) => {
                assert!(e.to_user_message().contains("non-empty"));
            }
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_no_fact_arg() {
        let tool = Remember::with_store(temp_store());
        let args = serde_json::json!({ "category": "project" });
        match tool.run(&ToolContext::new(), args).await {
            ToolOutcome::Failure(e) => {
                assert!(e.to_user_message().contains("non-empty"));
            }
            other => panic!("expected Failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_category_is_user() {
        let store = temp_store();
        let tool = Remember::with_store(store.clone());
        let args = serde_json::json!({ "fact": "test default category" });
        let _ = tool.run(&ToolContext::new(), args).await;

        let slug = slugify_description("test default category");
        let found = store.get(&slug).expect("fact should exist");
        assert_eq!(found.metadata.get("type").map(|s| s.as_str()), Some("user"));
    }

    #[tokio::test]
    async fn custom_category() {
        let store = temp_store();
        let tool = Remember::with_store(store.clone());
        let args = serde_json::json!({ "fact": "test custom category", "category": "project" });
        let _ = tool.run(&ToolContext::new(), args).await;

        let slug = slugify_description("test custom category");
        let found = store.get(&slug).expect("fact should exist");
        assert_eq!(
            found.metadata.get("type").map(|s| s.as_str()),
            Some("project")
        );
    }

    #[tokio::test]
    async fn same_fact_twice_is_idempotent() {
        let store = temp_store();
        let tool = Remember::with_store(store.clone());

        let args = serde_json::json!({ "fact": "test idempotent fact" });
        let _ = tool.run(&ToolContext::new(), args.clone()).await;
        let _ = tool.run(&ToolContext::new(), args).await;

        let slug = slugify_description("test idempotent fact");
        let facts = store.all();
        let matching: Vec<_> = facts.iter().filter(|f| f.name == slug).collect();
        assert_eq!(
            matching.len(),
            1,
            "same fact stored twice should be one entry"
        );
        assert_eq!(matching[0].body, "test idempotent fact");
    }
}
