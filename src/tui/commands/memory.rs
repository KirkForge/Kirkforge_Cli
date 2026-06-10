//! `/memory` slash command — persistent semantic memory management.
//!
//! Subcommands:
//! - `/memory add <fact>` — store a new fact (type auto-detected from keywords)
//! - `/memory list` — show all stored facts
//! - `/memory search <query>` — search by keyword
//! - `/memory rm <name>` — delete a fact by its name slug
//!
//! Facts are stored as markdown files with YAML frontmatter in
//! `~/.local/share/kirkforge/memory/`. They're injected into the
//! system prompt each turn.

use crate::session::memory::MemoryStore;

/// Auto-detect the memory type from the fact text.
fn detect_type(text: &str) -> &str {
    let lower = text.to_lowercase();
    if lower.contains("feedback")
        || lower.contains("correction")
        || lower.contains("should")
        || lower.contains("always")
        || lower.contains("never")
    {
        "feedback"
    } else if lower.contains("project")
        || lower.contains("repo")
        || lower.contains("codebase")
        || lower.contains("architecture")
    {
        "project"
    } else if lower.contains("user")
        || lower.contains("preference")
        || lower.contains("setup")
        || lower.contains("environment")
    {
        "user"
    } else if lower.contains("docs")
        || lower.contains("url")
        || lower.contains("reference")
        || lower.contains("wiki")
        || lower.contains("api")
    {
        "reference"
    } else {
        "project" // default
    }
}

/// Handle the `/memory` slash command.
///
/// Dispatches to the appropriate subcommand and returns a display string.
pub fn handle_memory_command(args: &str) -> String {
    let store = MemoryStore::default_store();
    let trimmed = args.trim();

    if trimmed.is_empty() {
        return "Usage: /memory add <fact> | list | search <query> | rm <name>".into();
    }

    let (subcmd, rest) = match trimmed.split_once(' ') {
        Some((cmd, r)) => (cmd, r),
        None => (trimmed, ""),
    };

    match subcmd {
        "add" => {
            if rest.is_empty() {
                return "Usage: /memory add <fact text> — store a new fact".into();
            }
            let mtype = detect_type(rest);
            let name = crate::session::memory::slugify_description(rest);
            let name = if name.len() > 40 { &name[..40] } else { &name };
            let description = if rest.len() > 80 {
                format!("{}…", &rest[..77])
            } else {
                rest.to_string()
            };

            match store.upsert(name, &description, rest, mtype) {
                Ok(fact) => {
                    let _ = store.write_index();
                    format!(
                        "✅ Stored memory [{}] as `{}` (type: {})\n→ {}\n\n**Why:** {}",
                        fact.name, fact.name, mtype, fact.description,
                        if mtype == "feedback" {
                            "Feedback memories shape future responses."
                        } else if mtype == "user" {
                            "User memories inform personalisation."
                        } else if mtype == "reference" {
                            "Reference memories provide context links."
                        } else {
                            "Project memories track ongoing work."
                        }
                    )
                }
                Err(e) => format!("❌ Failed to store memory: {}", e),
            }
        }
        "list" => {
            let facts = store.all();
            if facts.is_empty() {
                return "No memories stored yet. Use `/memory add <fact>` to store one.".into();
            }
            let mut out = format!("Memories ({}):\n", facts.len());
            for fact in &facts {
                let mtype = fact.metadata.get("type").cloned().unwrap_or_default();
                out.push_str(&format!(
                    "  • {:<12} {:<20} {:<10} {}\n",
                    mtype, fact.name, "", fact.description
                ));
            }
            out.push_str("\nUse `/memory search <query>` to filter, `/memory rm <name>` to delete.");
            out
        }
        "search" | "find" => {
            if rest.is_empty() {
                return "Usage: /memory search <keyword>".into();
            }
            let results = store.search(rest);
            if results.is_empty() {
                return format!("No memories matching \"{}\".", rest);
            }
            let mut out = format!("Found {} memories matching \"{}\":\n", results.len(), rest);
            for fact in &results {
                let mtype = fact.metadata.get("type").cloned().unwrap_or_default();
                out.push_str(&format!(
                    "  • {:<12} {} — {}\n",
                    mtype, fact.name, fact.description
                ));
            }
            out
        }
        "rm" | "remove" | "delete" => {
            if rest.is_empty() {
                return "Usage: /memory rm <name>".into();
            }
            match store.delete(rest) {
                Ok(true) => format!("🗑 Deleted memory `{}`.", rest),
                Ok(false) => format!("❌ No memory named `{}`.", rest),
                Err(e) => format!("❌ Failed to delete: {}", e),
            }
        }
        _ => format!(
            "Unknown subcommand: {}\nUsage: /memory add <fact> | list | search <query> | rm <name>",
            subcmd
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Override the memory store root for testing.
    fn test_store(dir: &tempfile::TempDir) -> MemoryStore {
        MemoryStore::open(dir.path().to_path_buf()).unwrap()
    }

    #[test]
    fn test_detect_type_feedback() {
        assert_eq!(detect_type("You should always run cargo check first"), "feedback");
        assert_eq!(detect_type("Never use unwrap in this repo"), "feedback");
    }

    #[test]
    fn test_detect_type_user() {
        assert_eq!(detect_type("Kirk user preference: dark mode"), "user");
        assert_eq!(detect_type("User setup: 16GB laptop"), "user");
    }

    #[test]
    fn test_detect_type_reference() {
        assert_eq!(detect_type("API docs at https://docs.rs"), "reference");
        assert_eq!(detect_type("Reference: the CLAUDE.md format"), "reference");
    }

    #[test]
    fn test_detect_type_project() {
        assert_eq!(detect_type("This project uses ratatui for TUI"), "project");
        assert_eq!(detect_type("Architecture: single-threaded TUI"), "project");
    }

    #[test]
    fn test_detect_type_defaults_to_project() {
        assert_eq!(detect_type("Random fact with no keywords"), "project");
    }

    #[test]
    fn test_handle_memory_empty_returns_usage() {
        let out = handle_memory_command("");
        assert!(out.contains("Usage"));
    }

    #[test]
    fn test_handle_memory_add_invalid_returns_usage() {
        let out = handle_memory_command("add");
        assert!(out.contains("Usage"));
        assert!(out.contains("add"));
    }

    #[test]
    fn test_handle_memory_search_empty_returns_usage() {
        let out = handle_memory_command("search");
        assert!(out.contains("Usage"));
    }

    #[test]
    fn test_handle_memory_rm_empty_returns_usage() {
        let out = handle_memory_command("rm");
        assert!(out.contains("Usage"));
    }

    #[test]
    fn test_handle_memory_unknown_subcommand() {
        let out = handle_memory_command("foo");
        assert!(out.contains("Unknown"));
    }
}
