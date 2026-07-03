//! Transcript formatter for the TUI conversation.
//!
//! Converts the in-memory display list (`Vec<ConversationEntry>`) into a
//! human-readable GitHub-flavored Markdown file. This is the format the
//! user can attach to another conversation or open in a text editor.

use crate::tui::app::ConversationEntry;

/// Format the visible conversation as a GitHub-flavored Markdown transcript.
///
/// The output is intentionally simple:
/// - Each entry becomes an `## <role> — <timestamp>` section.
/// - Assistant/user/system content is emitted as raw markdown so inline
///   code, lists, and emphasis remain readable.
/// - Tool entries include the visible summary plus the full sidecar output
///   in a fenced `text` block.
pub fn format_transcript(session_id: &str, entries: &[ConversationEntry]) -> String {
    let mut out = String::new();
    out.push_str("# KirkForge transcript — ");
    out.push_str(session_id);
    out.push('\n');
    out.push_str("\nGenerated: ");
    out.push_str(
        &chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S %:z")
            .to_string(),
    );
    out.push('\n');

    if entries.is_empty() {
        out.push_str("\n_(No messages yet.)_\n");
        out.push_str("\n---\n");
        return out;
    }

    for entry in entries {
        out.push('\n');
        out.push_str("## ");
        out.push_str(&entry.role);
        out.push_str(" — ");
        out.push_str(&entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string());
        out.push('\n');

        let content = entry.content.trim();
        if !content.is_empty() {
            out.push('\n');
            out.push_str(content);
            out.push('\n');
        }

        if let Some(full) = &entry.tool_output {
            out.push('\n');
            out.push_str("```text\n");
            out.push_str(full);
            if !full.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
    }

    out.push_str("\n---\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::ConversationEntry;

    #[test]
    fn empty_transcript_has_header_and_footer() {
        let s = format_transcript("2026-06-22-session-01", &[]);
        assert!(s.starts_with("# KirkForge transcript — 2026-06-22-session-01\n"));
        assert!(s.contains("(No messages yet.)"));
        assert!(s.ends_with("---\n"));
    }

    #[test]
    fn user_and_assistant_renders_as_sections() {
        let entries = vec![
            ConversationEntry::new("user", "hello"),
            ConversationEntry::new("assistant", "hi there"),
        ];
        let s = format_transcript("s1", &entries);
        assert!(s.contains("## user"));
        assert!(s.contains("## assistant"));
        assert!(s.contains("hello"));
        assert!(s.contains("hi there"));
    }

    #[test]
    fn tool_entry_includes_summary_and_full_output() {
        let entries = vec![ConversationEntry::tool(
            "🔧 bash (done) — 3 lines, 42 bytes",
            "line1\nline2\nline3",
        )];
        let s = format_transcript("s1", &entries);
        assert!(s.contains("## tool"));
        assert!(s.contains("🔧 bash (done)"));
        assert!(s.contains("```text"));
        assert!(s.contains("line1\nline2\nline3"));
        assert!(s.contains("```\n"));
    }

    #[test]
    fn tool_output_gets_trailing_newline_if_missing() {
        let entries = vec![ConversationEntry::tool("summary", "no-trailing-newline")];
        let s = format_transcript("s1", &entries);
        assert!(s.contains("no-trailing-newline\n```"));
    }

    #[test]
    fn system_messages_included() {
        let entries = vec![ConversationEntry::new("system", "Status update")];
        let s = format_transcript("s1", &entries);
        assert!(s.contains("## system"));
        assert!(s.contains("Status update"));
    }
}
