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
    /// Write a checkpoint every N messages. 0 disables message-count
    /// checkpointing; the log is still checkpointed after each completed
    /// tool batch by the executor.
    checkpoint_interval: usize,
    /// Messages appended since the last periodic checkpoint.
    messages_since_checkpoint: usize,
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
            let (loaded, corrupt) = match load_messages(&path) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read conversation log; attempting checkpoint restore");
                    (Vec::new(), true)
                }
            };

            if !loaded.is_empty() {
                if corrupt {
                    tracing::warn!(
                        path = %path.display(),
                        "conversation log contained corrupt lines; keeping valid messages"
                    );
                }
                (loaded, OpenOutcome::Loaded)
            } else if corrupt {
                tracing::warn!(path = %path.display(), "conversation log corrupt, attempting checkpoint restore");
                let mut log = Self {
                    path: path.clone(),
                    messages: Vec::new(),
                    checkpoint_interval: 0,
                    messages_since_checkpoint: 0,
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
            } else {
                // File exists but is empty/whitespace only.
                (Vec::new(), OpenOutcome::Loaded)
            }
        } else {
            (Vec::new(), OpenOutcome::Created)
        };

        Ok((
            Self {
                path,
                messages,
                checkpoint_interval: 0,
                messages_since_checkpoint: 0,
            },
            outcome,
        ))
    }

    /// Async version of [`open`]: offloads directory creation and log loading
    /// (including checkpoint recovery) to a dedicated thread pool.
    pub async fn open_async(path: PathBuf) -> anyhow::Result<(Self, OpenOutcome)> {
        tokio::task::spawn_blocking(move || Self::open(path))
            .await
            .context("conversation log open task panicked")?
    }

    /// Configure how often a checkpoint is written based on message count.
    ///
    /// Returns `self` so callers can chain after `ConversationLog::open`.
    pub fn with_checkpoint_interval(mut self, interval: usize) -> Self {
        self.checkpoint_interval = interval;
        self.messages_since_checkpoint = 0;
        self
    }

    /// Append a message to the log (both in-memory and on disk).
    ///
    /// Durability: the line is written and the file is flushed with
    /// `sync_all` before the in-memory vector is updated, so a crash
    /// loses at most the message currently being appended.
    pub fn append(&mut self, msg: Message) -> anyhow::Result<()> {
        let line = serde_json::to_string(&msg)?;
        let bytes = format!("{line}\n");
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open conversation log {} for append", self.path.display()))?;
        file.write_all(bytes.as_bytes())
            .with_context(|| format!("append to conversation log {}", self.path.display()))?;
        file.sync_all()?;
        self.messages.push(msg);

        if self.checkpoint_interval > 0 {
            self.messages_since_checkpoint += 1;
            if self.messages_since_checkpoint >= self.checkpoint_interval {
                self.checkpoint()?;
                self.messages_since_checkpoint = 0;
            }
        }
        Ok(())
    }

    /// Async version of [`append`]: offloads the blocking disk write to a
    /// dedicated thread pool so the Tokio runtime keeps making progress.
    pub async fn append_async(&mut self, msg: Message) -> anyhow::Result<()> {
        let line = serde_json::to_string(&msg)?;
        let bytes = format!("{line}\n");
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("open conversation log {} for append", path.display()))?;
            file.write_all(bytes.as_bytes())
                .with_context(|| format!("append to conversation log {}", path.display()))?;
            file.sync_all()?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("conversation log append task panicked")??;
        self.messages.push(msg);

        if self.checkpoint_interval > 0 {
            self.messages_since_checkpoint += 1;
            if self.messages_since_checkpoint >= self.checkpoint_interval {
                self.checkpoint_async().await?;
                self.messages_since_checkpoint = 0;
            }
        }
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

    /// Async version of [`checkpoint`]: offloads the blocking copy and prune
    /// operations to a dedicated thread pool.
    pub async fn checkpoint_async(&self) -> anyhow::Result<PathBuf> {
        const MAX_CHECKPOINTS: usize = 5;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = self.path.clone();
        let messages = self.messages.clone();
        tokio::task::spawn_blocking(move || {
            let filename = path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|f| format!("{f}.checkpoint-{nanos}.ndjson"))
                .unwrap_or_else(|| format!("conversation.checkpoint-{nanos}.ndjson"));
            let checkpoint_path = path.with_file_name(filename);
            Self::write_atomic_static(&checkpoint_path, &messages)?;

            if let Some(parent) = path.parent() {
                let prefix = checkpoint_prefix(&path);
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

            Ok::<_, anyhow::Error>(checkpoint_path)
        })
        .await
        .context("conversation log checkpoint task panicked")?
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
                Ok((messages, _)) if !messages.is_empty() => {
                    self.write_atomic(&self.path, &messages)?;
                    self.messages = messages;
                    return Ok(true);
                }
                Ok(_) => {
                    tracing::warn!(checkpoint = %checkpoint.display(), "checkpoint contained no valid messages, trying older");
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
        Self::write_atomic_static(path, messages)
    }

    /// Static variant of [`write_atomic`] so it can be used inside
    /// `spawn_blocking` closures that do not capture `self`.
    fn write_atomic_static(path: &std::path::Path, messages: &[Message]) -> anyhow::Result<()> {
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

    /// Async version of [`replace_all`]: offloads the blocking atomic rewrite
    /// to a dedicated thread pool.
    pub async fn replace_all_async(&mut self, messages: Vec<Message>) -> anyhow::Result<()> {
        let path = self.path.clone();
        let messages_for_task = messages.clone();
        tokio::task::spawn_blocking(move || Self::write_atomic_static(&path, &messages_for_task))
            .await
            .context("conversation log replace_all task panicked")??;
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
/// Valid JSON lines are parsed and returned; corrupt lines are skipped so
/// later valid lines are not lost. The returned bool is `true` when any
/// line could not be parsed. A file that contains at least one non-empty
/// line but zero parseable messages is treated as corrupt so callers can
/// fall back to checkpoint recovery. A file that is empty or contains only
/// whitespace is treated as an empty log.
///
/// Parses line-by-line from a buffered reader instead of slurping the whole
/// file into a single `String`, so opening a very large log does not allocate
/// more than one line at a time (P9).
fn load_messages(path: &std::path::Path) -> anyhow::Result<(Vec<Message>, bool)> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)
        .with_context(|| format!("open conversation log {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut corrupt = false;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .with_context(|| format!("read conversation log {}", path.display()))?;
        if n == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Message>(&line) {
            Ok(m) => messages.push(m),
            Err(e) => {
                corrupt = true;
                tracing::warn!(
                    path = %path.display(),
                    line = %line.trim(),
                    error = %e,
                    "skipping corrupt conversation log line"
                );
            }
        }
    }
    Ok((messages, corrupt))
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
    fn test_open_keeps_valid_lines_after_corrupt_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.ndjson");
        let lines = [
            r#"{"role":"user","content":"hello"}"#,
            "this is not json",
            r#"{"role":"assistant","content":"hi"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let (log, outcome) = ConversationLog::open(path).unwrap();
        // Valid lines before and after the corrupt line must be preserved;
        // only the corrupt line is dropped.
        assert_eq!(outcome, OpenOutcome::Loaded);
        assert_eq!(log.len(), 2);
        assert_eq!(log.messages[0].content, "hello");
        assert_eq!(log.messages[1].content, "hi");
    }

    #[test]
    fn test_open_restores_from_checkpoint_when_corrupt_line_seen() {
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

        // Append a second message, then corrupt the main log after it.
        log.append(Message {
            role: crate::shared::Role::User,
            content: "second".into(),
            ..Default::default()
        })
        .unwrap();
        std::fs::write(&path, "not json").unwrap();

        let (restored, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::Restored(1));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.last().unwrap().content, "first");
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
    fn test_periodic_checkpoint_every_n_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");

        let (mut log, _outcome) = ConversationLog::open(path.clone()).unwrap();
        log = log.with_checkpoint_interval(3);
        for i in 0..5 {
            log.append(Message {
                role: crate::shared::Role::User,
                content: format!("msg {i}"),
                ..Default::default()
            })
            .unwrap();
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
        assert!(
            !checkpoints.is_empty(),
            "interval=3 should produce at least one checkpoint after 5 appends"
        );
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

    /// Edge case: if the most recent checkpoint is corrupt, recovery should
    /// fall back to the next older intact checkpoint.
    #[test]
    fn test_open_falls_back_to_older_checkpoint_when_latest_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");

        let (mut log, _outcome) = ConversationLog::open(path.clone()).unwrap();
        log.append(Message {
            role: crate::shared::Role::User,
            content: "first".into(),
            ..Default::default()
        })
        .unwrap();
        let first_checkpoint = log.checkpoint().unwrap();

        log.append(Message {
            role: crate::shared::Role::User,
            content: "second".into(),
            ..Default::default()
        })
        .unwrap();
        let second_checkpoint = log.checkpoint().unwrap();

        // Corrupt the main log and the newest checkpoint.
        std::fs::write(&path, "not json").unwrap();
        std::fs::write(&second_checkpoint, "not json").unwrap();

        let (restored, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::Restored(1));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.last().unwrap().content, "first");
        assert!(first_checkpoint.exists());
    }

    /// Edge case: a partially truncated final NDJSON line is treated as
    /// corrupt, falling back to checkpoint recovery instead of silently
    /// dropping the trailing fragment.
    #[test]
    fn test_open_handles_truncated_final_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");

        let (mut log, _outcome) = ConversationLog::open(path.clone()).unwrap();
        log.append(Message {
            role: crate::shared::Role::User,
            content: "first".into(),
            ..Default::default()
        })
        .unwrap();
        log.checkpoint().unwrap();

        // Append a valid line, then truncate mid-way through the next object.
        let partial = r#"{"role":"user","content":""#;
        let first_json = serde_json::to_string(log.last().unwrap()).unwrap();
        std::fs::write(&path, format!("{first_json}\n{partial}")).unwrap();

        let (restored, outcome) = ConversationLog::open(path).unwrap();
        // The valid first line is kept; the truncated partial line is dropped.
        assert_eq!(outcome, OpenOutcome::Loaded);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.last().unwrap().content, "first");
    }

    /// Edge case: a log file containing only whitespace is treated as empty
    /// rather than corrupt.
    #[test]
    fn test_open_whitespace_only_log_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.conv.ndjson");
        std::fs::write(&path, "   \n\n\t\n").unwrap();

        let (log, outcome) = ConversationLog::open(path).unwrap();
        assert_eq!(outcome, OpenOutcome::Loaded);
        assert!(log.is_empty());
    }
}
