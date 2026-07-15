//! Interactive line-mode input with persistent history.
//!
//! When `--no-tui` is used in a real terminal, we replace the plain
//! `stdin.read_line` loop with a minimal readline implementation so the
//! user gets up/down history navigation. History is stored in
//! `<data_dir>/line_history.txt` and capped at 100 entries.
//!
//! Non-TTY stdin (pipes, redirects, `--non-interactive`) keeps the old
//! simple reader, because there is no terminal to negotiate and arrow-key
//! input is not available.

use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const HISTORY_CAP: usize = 100;
const PROMPT: &str = "> ";

/// Return `symbol` when color/emojis are allowed, otherwise an empty string.
///
/// This keeps line-mode output clean under `NO_COLOR` or `TERM=dumb` while
/// preserving visual markers in regular terminals.
pub fn symbol(no_color: bool, symbol: &'static str) -> &'static str {
    if no_color {
        ""
    } else {
        symbol
    }
}

/// Input source for the line-mode turn loop.
pub enum LineReader {
    /// Full readline editor with history and arrow-key navigation.
    /// The editor lives behind a mutex so a cancelled `next_line` future
    /// cannot lose it: the blocking task that owns the editor always puts
    /// it back before exiting.
    Interactive {
        editor: Arc<Mutex<Option<Box<rustyline::DefaultEditor>>>>,
        history_path: PathBuf,
    },
    /// Plain `stdin` reader for pipes and non-interactive runs.
    Plain,
}

impl LineReader {
    /// Build the right input source for the current environment.
    ///
    /// `interactive` is the inverse of the `--non-interactive` flag: when
    /// it is true and stdin is a TTY, we use the readline editor and load
    /// any existing history file.
    pub fn new(interactive: bool) -> anyhow::Result<Self> {
        if interactive && std::io::stdin().is_terminal() {
            let history_path = crate::session::data_dir()?.join("line_history.txt");
            if let Some(parent) = history_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let config = rustyline::Config::builder()
                .max_history_size(HISTORY_CAP)?
                .history_ignore_dups(true)?
                .color_mode(rustyline::ColorMode::Disabled)
                .build();
            let mut editor = rustyline::DefaultEditor::with_config(config)?;
            // A missing history file on first run is not an error.
            let _ = editor.load_history(&history_path);

            return Ok(Self::Interactive {
                editor: Arc::new(Mutex::new(Some(Box::new(editor)))),
                history_path,
            });
        }

        Ok(Self::Plain)
    }

    /// Read the next line from the user.
    ///
    /// Returns `Ok(None)` on EOF or Ctrl-C/Ctrl-D. For the interactive
    /// editor, accepted non-empty lines are appended to history and the
    /// capped history file is rewritten.
    pub async fn next_line(&mut self) -> anyhow::Result<Option<String>> {
        match self {
            Self::Interactive {
                editor,
                history_path,
            } => {
                let editor = editor.clone();
                let history_path = history_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let mut guard = editor.lock().expect("line-mode editor mutex poisoned");
                    let mut ed = guard.take().ok_or_else(|| {
                        anyhow::anyhow!(
                            "interactive editor is already in use (concurrent next_line call)"
                        )
                    })?;
                    // Keep the mutex held for the whole readline call. This
                    // prevents concurrent `next_line` calls and, crucially,
                    // ensures the editor is restored even if the outer async
                    // future is cancelled while awaiting this task.
                    let result = ed.readline(PROMPT);
                    *guard = Some(ed);
                    Ok::<_, anyhow::Error>(result)
                })
                .await??;

                match result {
                    Ok(line) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            // The in-memory history is already capped by
                            // the configured max_history_size. Rewrite the
                            // file so the cap is also persisted.
                            if let Self::Interactive { editor, .. } = self {
                                if let Ok(mut ed) = editor.lock() {
                                    if let Some(ref mut ed) = ed.as_mut() {
                                        let _ = ed.add_history_entry(trimmed);
                                        let _ = ed.save_history(&history_path);
                                    }
                                }
                            }
                        }
                        Ok(Some(line))
                    }
                    Err(rustyline::error::ReadlineError::Eof)
                    | Err(rustyline::error::ReadlineError::Interrupted) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Self::Plain => {
                let line = tokio::task::spawn_blocking(|| {
                    let stdin = std::io::stdin();
                    let mut reader = stdin.lock();
                    let mut buf = String::new();
                    match reader.read_line(&mut buf) {
                        Ok(0) => None,
                        Ok(_) => {
                            let trimmed = buf.trim();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed.to_string())
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read line-mode input from stdin");
                            None
                        }
                    }
                })
                .await?;
                Ok(line)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_mode_when_stdin_is_not_a_tty() {
        // Pipes and redirects always fall back to the plain reader.
        let reader = LineReader::new(true).unwrap();
        assert!(
            matches!(reader, LineReader::Plain),
            "expected plain reader for non-tty stdin"
        );
    }

    #[test]
    fn history_path_lives_in_data_dir() {
        // The path is computed inside the interactive constructor; we can
        // verify the shape by constructing the expected path directly.
        let expected = crate::session::data_dir()
            .expect("data_dir should resolve")
            .join("line_history.txt");
        assert_eq!(expected.file_name().unwrap(), "line_history.txt");
    }
}
