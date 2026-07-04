// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

use crate::shared::Message;
use anyhow::Context;
use std::path::PathBuf;

/// Outcome of opening a conversation log.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenOutcome {
    /// Existing log loaded intact.
    Loaded,
    /// New empty log was created because the file did not exist.
    Created,
    /// Log was unreadable and no usable checkpoint existed; started empty.
    StartedEmpty,
    /// Log was restored from the most recent intact checkpoint.
    /// Carries the number of recovered messages.
    Restored(usize),
}

/// Append-only conversation log.
/// Each message is appended to a file as a JSON line (NDJSON).
/// This format is crash-safe: a power cut loses at most one partial line.
pub struct ConversationLog {
    path: PathBuf,
    messages: Vec<Message>,
}

impl ConversationLog {
    /// Open or create a conversation log at the given path.
    ///
    /// If the file exists but is corrupt, this automatically attempts to
    /// restore from the most recent intact checkpoint. The returned
    /// `OpenOutcome` indicates whether the log was restored so callers can
    /// surface a notice to the user.
    pub fn open(path: PathBuf) -> anyhow::Result<(Self, OpenOutcome)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create conversation log directory {}", parent.display())
            })?;
        }

        let (messages, outcome) = if path.exists() {
            match load_messages(&path) {
                Ok(messages) => (messages, OpenOutcome::Loaded),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "conversation log corrupt, attempting checkpoint restore");
                    let mut log = Self {
                        path: path.clone(),
                        messages: Vec::new(),
                    };
                    match log.restore_from_checkpoint() {
                        Ok(true) => {
                            tracing::info!(path = %path.display(), "restored conversation from checkpoint");
                            (log.messages.clone(), OpenOutcome::Restored(log.len()))
                        }
                        Ok(false) => {
                            tracing::warn!(path = %path.display(), "no checkpoint available; starting empty");
                            (Vec::new(), OpenOutcome::StartedEmpty)
                        }
                        Err(restore_err) => {
                            tracing::error!(path = %path.display(), error = %restore_err, "checkpoint restore failed; starting empty");
                            (Vec::new(), OpenOutcome::StartedEmpty)
                        }
                    }
                }
            }
        } else {
            (Vec::new(), OpenOutcome::Created)
        };

        Ok((Self { path, messages }, outcome))
    }

    /// Append a message to the log (both in-memory and on disk).
    ///
    /// Durability: the line is written and the file is flushed with
    /// `sync_all` before the in-memory vector is updated, so a crash
    /// loses at most the message currently being appended.
    pub fn append(&mut self, msg: Message) -> anyhow::Result<()> {
        let line = serde_json::to_string(&msg)?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open conversation log {} for append", self.path.display()))?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        self.messages.push(msg);
        Ok(())
    }

    /// Create a timestamped checkpoint backup of the current log.
    ///
    /// Checkpoints live next to the main log as
    /// `<stem>.checkpoint-<timestamp>.ndjson` and are capped to a small
    /// number by removing the oldest. They allow recovery from a corrupt
    /// or partially truncated main log by restoring from the most
    /// recent intact checkpoint.
    pub fn checkpoint(&self) -> anyhow::Result<PathBuf> {
        const MAX_CHECKPOINTS: usize = 5;

        // Use nanosecond-resolution epoch time so rapid checkpoint creation
        // in tight loops cannot collide.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let filename = self
            .path
            .file_name()
            .and_then(|f| f.to_str())
            .map(|f| format!("{f}.checkpoint-{nanos}.ndjson"))
            .unwrap_or_else(|| format!("conversation.checkpoint-{nanos}.ndjson"));
        let checkpoint_path = self.path.with_file_name(filename);
        self.write_atomic(&checkpoint_path, &self.messages)?;

        // Prune old checkpoints, keeping the most recent `MAX_CHECKPOINTS`.
        if let Some(parent) = self.path.parent() {
            let prefix = checkpoint_prefix(&self.path);
            let mut checkpoints: Vec<PathBuf> = std::fs::read_dir(parent)
                .with_context(|| format!("list checkpoints in {}", parent.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|f| f.to_str())
                        .map(|f| f.starts_with(&prefix) && f.ends_with(".ndjson"))
                        .unwrap_or(false)
                })
                .collect();
            checkpoints.sort();
            while checkpoints.len() > MAX_CHECKPOINTS {
                let oldest = checkpoints.remove(0);
                if let Err(e) = std::fs::remove_file(&oldest) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            error = %e,
                            path = %oldest.display(),
                            "Failed to prune old conversation checkpoint"
                        );
                    }
                }
            }
        }

        Ok(checkpoint_path)
    }

    /// Restore from the most recent intact checkpoint file.
    ///
    /// Returns `Ok(true)` if a checkpoint was found and restored,
    /// `Ok(false)` if no checkpoint exists. If the latest checkpoint is
    /// also corrupt, older checkpoints are tried in turn.
    pub fn restore_from_checkpoint(&mut self) -> anyhow::Result<bool> {
        let Some(parent) = self.path.parent() else {
            return Ok(false);
        };
        let prefix = checkpoint_prefix(&self.path);
        let mut checkpoints: Vec<PathBuf> = std::fs::read_dir(parent)
            .with_context(|| format!("list checkpoints in {}", parent.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .map(|f| f.starts_with(&prefix) && f.ends_with(".ndjson"))
                    .unwrap_or(false)
            })
            .collect();
        checkpoints.sort();
        checkpoints.reverse();

        for checkpoint in &checkpoints {
            match load_messages(checkpoint) {
                Ok(messages) => {
                    self.write_atomic(&self.path, &messages)?;
                    self.messages = messages;
                    return Ok(true);
                }
                Err(e) => {
                    tracing::warn!(checkpoint = %checkpoint.display(), error = %e, "checkpoint corrupt, trying older");
                }
            }
        }
        Ok(false)
    }

    /// Write `messages` to `path` atomically via temp file + rename.
    fn write_atomic(&self, path: &std::path::Path, messages: &[Message]) -> anyhow::Result<()> {
        use std::io::Write;
        let tmp_path = path.with_extension("ndjson.tmp");
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)
                .with_context(|| {
                    format!("create temporary conversation log {}", tmp_path.display())
                })?;
            for msg in messages {
                let line = serde_json::to_string(msg)?;
                writeln!(file, "{line}")?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "commit conversation log from {} to {}",
                tmp_path.display(),
                path.display()
            )
        })?;
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
        self.write_atomic(&self.path, &messages)?;
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

    /// True if the log contains no messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
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

/// Load messages from an NDJSON file.
///
/// Valid JSON lines are parsed and returned. A file that contains at
/// least one non-empty line but zero parseable messages is treated as
/// corrupt so that callers can fall back to checkpoint recovery. A file
/// that is empty or contains only whitespace is treated as an empty log.
fn load_messages(path: &std::path::Path) -> anyhow::Result<Vec<Message>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read conversation log {}", path.display()))?;
    let mut messages = Vec::new();
    let mut had_non_empty_line = false;
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        had_non_empty_line = true;
        match serde_json::from_str::<Message>(line) {
            Ok(m) => messages.push(m),
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "skipping corrupt log line");
            }
        }
    }
    if messages.is_empty() && had_non_empty_line {
        anyhow::bail!("conversation log contains no valid messages");
    }
    Ok(messages)
}

/// Checkpoint filename prefix for a given conversation log path.
/// Returns e.g. `session.conv.ndjson.checkpoint-` for `session.conv.ndjson`.
fn checkpoint_prefix(path: &std::path::Path) -> String {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("conversation");
    format!("{filename}.checkpoint-")
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

        let (log, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::Loaded);
        let messages: Vec<&Message> = log.all().iter().collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, crate::shared::Role::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, crate::shared::Role::Assistant);
        assert_eq!(messages[1].content, "hi");
    }

    #[test]
    fn test_open_restores_from_checkpoint_when_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");

        let (mut log, _outcome) = ConversationLog::open(path.clone()).unwrap();
        log.append(Message {
            role: crate::shared::Role::User,
            content: "first".into(),
            ..Default::default()
        })
        .unwrap();

        let checkpoint = log.checkpoint().unwrap();
        assert!(checkpoint.exists());

        // Simulate main log corruption.
        std::fs::write(&path, "not json").unwrap();
        let (restored, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::Restored(1));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.last().unwrap().content, "first");
    }

    #[test]
    fn test_open_starts_empty_when_corrupt_and_no_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");
        std::fs::write(&path, "not json").unwrap();

        let (restored, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::StartedEmpty);
        assert!(restored.is_empty());
    }

    #[test]
    fn test_checkpoint_and_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");

        let (mut log, _outcome) = ConversationLog::open(path.clone()).unwrap();
        log.append(Message {
            role: crate::shared::Role::User,
            content: "first".into(),
            ..Default::default()
        })
        .unwrap();

        let checkpoint = log.checkpoint().unwrap();
        assert!(checkpoint.exists());

        // Simulate main log corruption.
        std::fs::write(&path, "not json").unwrap();
        let (mut restored, _outcome) = ConversationLog::open(path).unwrap();
        assert!(restored.restore_from_checkpoint().unwrap());
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.last().unwrap().content, "first");
    }

    #[test]
    fn test_checkpoint_pruning_keeps_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");
        let (mut log, _outcome) = ConversationLog::open(path).unwrap();

        for i in 0..7 {
            log.append(Message {
                role: crate::shared::Role::User,
                content: format!("msg {i}"),
                ..Default::default()
            })
            .unwrap();
            log.checkpoint().unwrap();
        }

        let checkpoints: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .map(|f| {
                        f.starts_with("session.conv.ndjson.checkpoint-") && f.ends_with(".ndjson")
                    })
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(checkpoints.len(), 5, "oldest checkpoints should be pruned");
    }
}
