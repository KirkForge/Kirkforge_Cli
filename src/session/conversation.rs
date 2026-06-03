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
                .filter_map(|l| serde_json::from_str::<Message>(l).ok())
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
        writeln!(file, "{}", line)?;
        self.messages.push(msg);
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
