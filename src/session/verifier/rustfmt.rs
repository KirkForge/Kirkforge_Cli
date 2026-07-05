use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
/// Rustfmt verifier — checks formatting for edited Rust files.
///
/// Subscribes to `Edit` and `FileWrite` events. For Rust files, it runs
/// `rustfmt --check <file>`. If the file is not formatted, it returns a
/// `Fixable` suggestion with `command = Some("rustfmt")` and empty
/// `original`/`replacement`, so the correction loop runs `rustfmt <file>`.
///
/// The rustfmt verifier is registered at priority 4 (after lint).
use crate::session::verifier::{FixSuggestion, Verdict};

/// Run the rustfmt verifier against an event.
pub async fn verify_rustfmt(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    let is_rust = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "rs");
    if !is_rust {
        return Verdict::Skipped(format!("not a Rust file: {}", path.display()));
    }

    let output = match tokio::process::Command::new("rustfmt")
        .arg("--check")
        .arg(&path)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("rustfmt not available: {e}")),
    };

    if output.status.success() {
        return Verdict::Clean;
    }

    Verdict::Fixable(FixSuggestion {
        description: format!("{} is not formatted", path.display()),
        file: path,
        original: String::new(),
        replacement: String::new(),
        severity: "warning".into(),
        command: Some("rustfmt".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skips_non_edit_events() {
        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 3,
            stderr_len: 0,
            workdir: None,
        });
        let v = verify_rustfmt(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_skips_non_rust_files() {
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: std::path::PathBuf::from("readme.md"),
            content_length: 10,
        });
        let v = verify_rustfmt(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_formatted_file_is_clean() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_rustfmt_clean.rs");
        std::fs::write(&path, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 30,
        });
        let v = verify_rustfmt(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "formatted file should be clean: {v:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_unformatted_file_returns_rustfmt_command() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_rustfmt_dirty.rs");
        // Missing space after `fn` so rustfmt will want to rewrite it.
        std::fs::write(&path, "fn  main(){println!(\"hi\");}\n").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 32,
        });
        let v = verify_rustfmt(&event).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert_eq!(suggestion.command.as_deref(), Some("rustfmt"));
                assert!(suggestion.original.is_empty());
                assert!(suggestion.replacement.is_empty());
                assert!(suggestion.description.contains("not formatted"));
            }
            other => panic!("expected Fixable rustfmt suggestion, got {other:?}"),
        }

        let _ = std::fs::remove_file(&path);
    }
}
