//! Terminal setup / teardown helper.
//!
//! Wraps `ratatui::init` / `restore` so callers don't have to think
//! about cleanup. The `TerminalGuard` restores the original terminal
//! state on drop (including on panic), so we never leave the user's
//! shell in raw mode.
//!
//! `ratatui::DefaultTerminal` (added in 0.29, still
//! present in 0.30) provides a similar guard for the
//! common case, but we keep our own wrapper so we can
//! run a controlled init sequence (raw mode + bracketed
//! paste enable + alternate screen enter + line wrap
//! disable) before constructing the terminal, with no
//! mouse capture until the editor opts in.

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{IsTerminal, Stdout};

/// Owns the terminal while the TUI is running. Drops back to cooked
/// mode and shows the cursor when it goes out of scope.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Initialize a new ratatui terminal in raw mode with the given
    /// title hidden (we set it ourselves via the UI). Returns the
    /// guard; dropping it restores the terminal.
    pub fn new() -> Result<Self> {
        if !std::io::stdout().is_terminal() {
            anyhow::bail!(
                "interactive mode requires a terminal; use --render for non-interactive output"
            );
        }
        crossterm::terminal::enable_raw_mode().context("enable raw mode")?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::event::EnableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::terminal::DisableLineWrap,
        )
        .context("enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("create ratatui terminal")?;
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore. If any of these fail we've already
        // lost the terminal; log to stderr and move on.
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::terminal::EnableLineWrap,
        );
        crossterm::terminal::disable_raw_mode().ok();
    }
}
