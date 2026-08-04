// `kf-code sessions <args>` command (list / search / export).
// Extracted from the binary root — pure move, no behaviour change.

use std::path::PathBuf;

pub(super) fn handle_sessions_command(
    id: Option<String>,
    export: Option<String>,
    out_path: Option<PathBuf>,
    search: Option<String>,
) -> anyhow::Result<()> {
    use kf_code::session::conversation::ConversationLog;
    use kf_code::session::session_index::{list_sessions, resolve_session_id, search_sessions};
    use kf_code::{shared, tui};

    // Search takes priority over list when no id/export is given.
    if let Some(query) = search {
        let entries = search_sessions(&query).unwrap_or_default();
        if entries.is_empty() {
            println!("No sessions matching '{query}'.");
            return Ok(());
        }
        println!("{:<30} {:>6} {:>10}  started", "ID", "msgs", "size");
        println!("{}", "-".repeat(60));
        for e in &entries {
            println!(
                "{:<30} {:>6} {:>10}  {}",
                e.id,
                e.message_count,
                format!("{:.1} KB", e.size_bytes as f64 / 1024.0),
                e.started_at
            );
        }
        return Ok(());
    }

    // No id → list
    if id.is_none() && export.is_none() {
        let entries = list_sessions().unwrap_or_default();
        if entries.is_empty() {
            println!("No sessions found.");
            return Ok(());
        }
        println!("{:<30} {:>6} {:>10}  started", "ID", "msgs", "size");
        println!("{}", "-".repeat(60));
        for e in &entries {
            println!(
                "{:<30} {:>6} {:>10}  {}",
                e.id,
                e.message_count,
                format!("{:.1} KB", e.size_bytes as f64 / 1024.0),
                e.started_at
            );
        }
        return Ok(());
    }

    let id = id.ok_or_else(|| anyhow::anyhow!("--export requires a session id"))?;
    let fmt = export.as_deref().unwrap_or("markdown");

    let path =
        resolve_session_id(&id)?.ok_or_else(|| anyhow::anyhow!("session '{id}' not found"))?;

    let content = match fmt {
        "ndjson" => std::fs::read_to_string(&path)?,
        "json" => {
            let (log, _) = ConversationLog::open(path)?;
            serde_json::to_string_pretty(log.all())?
        }
        "markdown" | "md" => {
            let (log, _) = ConversationLog::open(path)?;
            // Build ConversationEntry list from Message list for transcript formatter
            let entries: Vec<tui::app::ConversationEntry> = log
                .all()
                .iter()
                .map(|m| {
                    let role = match m.role {
                        shared::Role::User => "user",
                        shared::Role::Assistant => "assistant",
                        shared::Role::Tool => "tool",
                        shared::Role::System => "system",
                    };
                    tui::app::ConversationEntry::new(role, m.content.clone())
                })
                .collect();
            tui::transcript::format_transcript(&id, &entries)
        }
        other => anyhow::bail!("unknown export format '{other}'; use markdown, json, or ndjson"),
    };

    if let Some(p) = out_path {
        std::fs::write(&p, &content)?;
        println!("Exported {} session to {}", fmt, p.display());
    } else {
        print!("{content}");
    }

    Ok(())
}
