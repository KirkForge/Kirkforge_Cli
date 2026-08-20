// `kf-code replay <args>` command + interactive replay TUI loop.
// Extracted from the binary root — pure move, no behaviour change.

pub(super) fn handle_replay_command(
    id: String,
    data_dir: Option<std::path::PathBuf>,
    turn: Option<u32>,
    from: Option<u32>,
    to: Option<u32>,
    interactive: bool,
) -> anyhow::Result<()> {
    use kf_code::session::replay::{format_turn, TraceRecorder};

    let data = data_dir.unwrap_or_else(|| {
        kf_code::session::data_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // Resolve session id to trace file path.
    let trace_path = if id.ends_with(".trace.ndjson") && std::path::Path::new(&id).exists() {
        std::path::PathBuf::from(id)
    } else {
        // Search sessions directory for matching id prefix.
        let sessions_dir = data.join("sessions");
        if sessions_dir.is_dir() {
            let mut found: Option<std::path::PathBuf> = None;
            for entry in std::fs::read_dir(&sessions_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&id) && name.ends_with(".trace.ndjson") {
                    found = Some(entry.path());
                    break;
                }
            }
            match found {
                Some(p) => p,
                None => {
                    // Try as direct path under data dir.
                    data.join(format!("{id}.trace.ndjson"))
                }
            }
        } else {
            data.join(format!("{id}.trace.ndjson"))
        }
    };

    if !trace_path.exists() {
        anyhow::bail!("trace file not found: {}", trace_path.display());
    }

    let records = TraceRecorder::load(&trace_path)?;
    if records.is_empty() {
        println!("No turns recorded in this session.");
        return Ok(());
    }

    // Filter by turn number / range.
    let filtered: Vec<_> = records
        .into_iter()
        .filter(|r| {
            if let Some(t) = turn {
                r.turn == t
            } else {
                let after_from = from.is_none_or(|f| r.turn >= f);
                let before_to = to.is_none_or(|t_| r.turn <= t_);
                after_from && before_to
            }
        })
        .collect();

    if filtered.is_empty() {
        println!("No turns match the specified filter.");
        return Ok(());
    }

    if interactive {
        // Interactive TUI stepper. Hand the filtered records to the
        // replay app and run a minimal ratatui loop.
        return run_replay_tui(filtered);
    }

    for record in &filtered {
        print!("{}", format_turn(record));
    }

    Ok(())
}

/// Run the interactive replay TUI over a pre-filtered set of records.
///
/// This mirrors the standalone session-picker loop in `tui::mod.rs`:
/// enable raw mode, enter the alternate screen, drive the `ReplayApp`
/// until it signals quit, then restore terminal state. Errors during
/// teardown are logged but not propagated (the user has already seen
/// the replay; a dirty terminal on exit is worse than a lost log line).
fn run_replay_tui(records: Vec<kf_code::session::replay::TurnRecord>) -> anyhow::Result<()> {
    use crossterm::{
        event::{self, Event},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use kf_code::session::replay::ReplayStepper;
    use kf_code::tui::replay::ReplayApp;
    use ratatui::{backend::CrosstermBackend, Terminal};

    // Same panic-containment contract as the main TUI (WO 38.2): the
    // hook must be in place before raw mode is enabled.
    kf_code::tui::install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = ReplayApp::new(ReplayStepper::new(records));

    let result = loop {
        terminal.draw(|f| app.render(f, f.area()))?;
        match event::read() {
            Ok(Event::Key(key)) => {
                app.handle_key(key);
                if app.should_quit() {
                    break Ok(());
                }
            }
            Ok(Event::Resize(_, _)) => {
                // Next draw will pick up the new size.
            }
            Ok(_) => {}
            Err(e) => {
                break Err(anyhow::anyhow!("terminal read error: {e}"));
            }
        }
    };

    // Teardown — best-effort, mirror the session-picker pattern.
    if let Err(e) = disable_raw_mode() {
        tracing::warn!(error = %e, "failed to disable raw mode during replay TUI teardown");
    }
    if let Err(e) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        tracing::warn!(error = %e, "failed to leave alternate screen during replay TUI teardown");
    }

    result
}
