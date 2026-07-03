// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

use crate::shared::Message;
use std::path::PathBuf;

/// Append-only conversation log.
/// Each message is appended to a file as a JSON line (NDJSON).
/// This format is crash-safe: a power cut loses at most one partial line.
pub struct ConversationLog {
    path: PathBuf,
    messages: Vec<Message>,
}

impl ConversationLog {
    /// Open or create a conversation log at the given path.
    /// If the file exists, it's loaded into memory.
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let messages = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| match serde_json::from_str::<Message>(l) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::warn!(error = %e, line = %l, "skipping corrupt conversation log line");
                        None
                    }
                })
                .collect()
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Vec::new()
        };

        Ok(Self { path, messages })
    }

    /// Append a message to the log (both in-memory and on disk).
    pub fn append(&mut self, msg: Message) -> anyhow::Result<()> {
        let line = serde_json::to_string(&msg)?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;
        self.messages.push(msg);
        Ok(())
    }

    /// Replace the in-memory message list and rewrite the log file
    /// atomically. Used by `/compact` to persist the compacted history
    /// so a future `conversation_log().all()` sees the new messages.
    ///
    /// Atomicity: we write to `<path>.tmp` first, then rename. A crash
    /// mid-rewrite either leaves the old log intact (if the rename
    /// hasn't happened) or the new log fully written — never a
    /// half-truncated file.
    ///
    /// The original full conversation is lost from the log (it lives
    /// on in the TUI's `ConversationEntry::tool_output` sidecars for
    /// tool results, and in `Role::Assistant` condensed messages as
    /// first-500-chars previews). This is by design — `/compact` is a
    /// destructive user action that explicitly opts in to losing
    /// context.
    pub fn replace_all(&mut self, messages: Vec<Message>) -> anyhow::Result<()> {
        use std::io::Write;
        let tmp_path = self.path.with_extension("ndjson.tmp");
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)?;
            for msg in &messages {
                let line = serde_json::to_string(msg)?;
                writeln!(file, "{line}")?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, &self.path)?;
        self.messages = messages;
        Ok(())
    }

    /// All messages in the conversation.
    pub fn all(&self) -> &[Message] {
        &self.messages
    }

    /// Total message count.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// The most recent message.
    pub fn last(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// Path to the log file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Message;

    #[test]
    fn test_open_skips_corrupt_lines_and_keeps_valid_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.ndjson");
        let lines = [
            r#"{"role":"user","content":"hello"}"#,
            "this is not json",
            r#"{"role":"assistant","content":"hi"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let log = ConversationLog::open(path).unwrap();
        let messages: Vec<&Message> = log.all().iter().collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, crate::shared::Role::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, crate::shared::Role::Assistant);
        assert_eq!(messages[1].content, "hi");
    }
}
