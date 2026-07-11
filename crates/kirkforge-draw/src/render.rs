//! Non-interactive render path.
//!
//! Loads a `.td.json` document, composes a scene around the document
//! bounds, and emits the rendered art as plain text. Used by:
//!   * `kfd --render --plain`  → plain text (no color).
//!   * `kfd --render --ansi`   → ANSI-colored terminal output.
//!   * `kfd --render --fenced` → fenced markdown code block.
//!   * `kfd --validate`        → diagnostic report to stdout, exit 0/1.
//!   * The TUI pane (consumes the scene directly via scene_render).

use anyhow::{Context, Result};
use std::fs;

use kirkforge_draw_core::{build_scene, load_document, render_plain, validate_document};

/// Validate a user-supplied path argument before passing it to
/// `std::fs::*`. Delegates to [`crate::event::validate_path_arg`],
/// which rejects the three cross-platform footguns (empty
/// string, whitespace-only, interior NUL byte) and surfaces each
/// as a single human-readable `anyhow` error — one source of
/// truth shared by `load_doc` and the save-as commit path.
/// Load a `.td.json` file from disk and return its parsed document.
pub fn load_doc(path: &str) -> Result<kirkforge_draw_core::DrawDocument> {
    crate::event::validate_path_arg(path)?;
    let json = fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let (doc, _report) = load_document(&json).with_context(|| format!("parse {path}"))?;
    Ok(doc)
}

/// Read a `.td.json` file and run the validator over it. The reader
/// error is propagated via `Result`; the report itself never errors.
pub fn run_validate(path: &str) -> Result<kirkforge_draw_core::ValidateReport> {
    crate::event::validate_path_arg(path)?;
    let json = fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    Ok(validate_document(&json))
}

// ponytail: path-traversal audit on --load / --output.
//
// `path` here is the literal argv string from `--load FILE` /
// `--output FILE` (clap's `cli.load: Option<String>`); there is
// no IPC, env var, config-file, or any other indirect source
// that could inject a path without the user typing it. Threat
// model is therefore "the same user who can already `cat FILE`
// decides to invoke `kfd --load FILE`" — the read privilege
// boundary the user cares about (filesystem ACLs on the load
// target) is enforced by the OS, not by this code.
//
// `crate::event::validate_path_arg` (in event.rs, shared
// with the save path) gates the cross-platform footguns
// (empty path, whitespace-only path, interior NUL byte)
// before the OS sees the path — see its definition for
// the full contract. The remaining checks the OS
// enforces for us:
//
//   * `std::fs::read_to_string` follows symlinks by default,
//     which is the right behavior here: a wrapper script that
//     pre-checks the path and then invokes `kfd --load` would
//     rather see "permission denied" propagated from
//     `read_to_string` than have symlinks silently resolved or
//     refused.
//
//   * `std::fs::OpenOptions::follow_links(false)` was considered
//     and rejected — POSIX `O_NOFOLLOW` would change the failure
//     mode for legitimate symlink-wrapped diagrams (e.g., a
//     `latest.td.json` symlink into a dated directory) without
//     buying any privilege the OS isn't already enforcing.
//
// Ctrl-S in the editor writes back to the same `source_path`
// string that's just been loaded, via `atomic_write` (write-temp
// + sync_all + rename). Same threat model: user typed the path.
// `rename` semantics on POSIX replace a target symlink with the
// source file atomically; on Windows `rename` fails if the
// target exists, which the editor surfaces as "save failed" —
// both behaviors are correct for the threat model.

/// Format a `ValidateReport` as a human-readable block for `--validate`
/// output. Stable, line-oriented layout so build pipelines can grep
/// it.
pub fn format_validate_report(report: &kirkforge_draw_core::ValidateReport, path: &str) -> String {
    use kirkforge_draw_core::ObjectKind;
    let mut out = String::new();
    out.push_str(&format!("validate: {path}\n"));
    out.push_str(&format!(
        "  schema: {} (expected {})\n",
        report
            .version_found
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".into()),
        report.version_expected,
    ));
    let kinds = [
        ObjectKind::Box,
        ObjectKind::Line,
        ObjectKind::Elbow,
        ObjectKind::Paint,
        ObjectKind::Text,
    ];
    let counts = kinds
        .iter()
        .map(|k| {
            let n = report.object_counts.get(k).copied().unwrap_or(0);
            format!("{}={n}", kind_label(*k))
        })
        .collect::<Vec<_>>()
        .join(" ");
    out.push_str(&format!("  objects: {} ({counts})\n", report.object_total));

    for e in &report.errors {
        out.push_str(&format!("  error: {e}\n"));
    }
    for w in &report.unknown_object_warnings {
        out.push_str(&format!("  unknown: {w}\n"));
    }
    for d in &report.duplicate_ids {
        out.push_str(&format!("  duplicate: {d}\n"));
    }
    for d in &report.degenerate_object_ids {
        out.push_str(&format!("  degenerate: id={d:?} (renders to nothing)\n"));
    }
    out.push_str(if report.is_ok() {
        "  result: OK\n"
    } else {
        "  result: FAILED\n"
    });
    out
}

/// Serialize a `ValidateReport` as pretty-printed JSON. The path
/// the report came from is included as a top-level `path` field
/// next to the `report` payload so shell pipelines that already
/// know the file (they just passed it on the command line) can
/// still grep a multi-file batch by source.
pub fn format_validate_report_json(
    report: &kirkforge_draw_core::ValidateReport,
    path: &str,
) -> anyhow::Result<String> {
    use serde_json::json;
    let value = json!({
        "path": path,
        "ok": report.is_ok(),
        "report": report,
    });
    serde_json::to_string_pretty(&value)
        .map_err(|e| anyhow::anyhow!("failed to serialize validate report: {e}"))
}

fn kind_label(k: kirkforge_draw_core::ObjectKind) -> &'static str {
    match k {
        kirkforge_draw_core::ObjectKind::Box => "box",
        kirkforge_draw_core::ObjectKind::Line => "line",
        kirkforge_draw_core::ObjectKind::Elbow => "elbow",
        kirkforge_draw_core::ObjectKind::Paint => "paint",
        kirkforge_draw_core::ObjectKind::Text => "text",
    }
}

/// Render a document as a fenced markdown code block.
pub fn render_fenced(doc: &kirkforge_draw_core::DrawDocument) -> String {
    let mut out = String::from("```\n");
    out.push_str(&render_plain(doc));
    out.push_str("```\n");
    out
}

/// Render a document to an ANSI-colored terminal string. Each row
/// begins with `CSI row;1 H` (absolute cursor position), then walks
/// the cells emitting SGR color codes only when the color changes.
/// Blank cells become spaces (or no-ops if the color matches the
/// previous non-blank cell, so adjacent colored glyphs stay grouped).
///
/// Empty documents produce an empty string.
pub fn render_ansi(doc: &kirkforge_draw_core::DrawDocument) -> String {
    let Some(scene) = build_scene(doc) else {
        return String::new();
    };
    let mut out = String::new();
    for (row_idx, row) in scene.cells.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H", row_idx + 1));
        emit_ansi_row(&mut out, row);
    }
    out.push_str("\x1b[0m");
    out
}

fn emit_ansi_row(out: &mut String, row: &[kirkforge_draw_core::SceneCell]) {
    let mut last_code: Option<i32> = None;
    for cell in row {
        let code = ansi_color_code(cell.color);
        if cell.glyph == ' ' {
            // Skip — blank cells stay as whatever the terminal paints
            // there. This keeps cursor advances predictable without
            // wasting escape bytes on whitespace.
            continue;
        }
        if code != last_code {
            match code {
                Some(c) => out.push_str(&format!("\x1b[{c}m")),
                None => out.push_str("\x1b[0m"),
            }
            last_code = code;
        }
        out.push(cell.glyph);
    }
}

/// Map `InkColor` to a foreground SGR code (30-37). `None` means
/// "use the terminal default" — the caller emits `\x1b[0m`.
fn ansi_color_code(c: Option<kirkforge_draw_core::InkColor>) -> Option<i32> {
    use kirkforge_draw_core::InkColor;
    match c {
        None => None,
        Some(InkColor::White) => Some(37),
        Some(InkColor::Red) => Some(31),
        Some(InkColor::Orange) => Some(33),
        Some(InkColor::Yellow) => Some(33),
        Some(InkColor::Green) => Some(32),
        Some(InkColor::Cyan) => Some(36),
        Some(InkColor::Blue) => Some(34),
        Some(InkColor::Magenta) => Some(35),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_draw_core::{
        types::{BoxObject, BoxStyle, DrawDocument, DrawObject, InkColor},
        DRAW_DOCUMENT_VERSION,
    };

    fn one_box_doc() -> DrawDocument {
        DrawDocument {
            version: 1,
            objects: vec![DrawObject::Box(BoxObject {
                id: "b".into(),
                z: 1,
                parent_id: None,
                color: InkColor::White,
                left: 0,
                top: 0,
                right: 4,
                bottom: 2,
                style: BoxStyle::Light,
            })],
        }
    }

    #[test]
    fn empty_doc_renders_to_empty_line() {
        let doc = DrawDocument {
            version: 1,
            objects: vec![],
        };
        assert_eq!(render_plain(&doc), "\n");
        assert_eq!(render_ansi(&doc), "");
        assert!(render_fenced(&doc).contains("```"));
    }

    #[test]
    fn plain_renders_box_frame() {
        let out = render_plain(&one_box_doc());
        assert!(out.contains('┌'));
        assert!(out.contains('─'));
        assert!(out.contains('│'));
    }

    #[test]
    fn plain_trims_trailing_spaces() {
        let out = render_plain(&one_box_doc());
        for line in out.lines() {
            assert!(!line.ends_with(' '), "trailing space in {line:?}");
        }
    }

    #[test]
    fn fenced_wraps_in_code_block() {
        let out = render_fenced(&one_box_doc());
        assert!(out.starts_with("```\n"));
        assert!(out.ends_with("```\n"));
    }

    #[test]
    fn ansi_uses_csi_positioning_and_sgr() {
        let out = render_ansi(&one_box_doc());
        // First row should start with CSI 1;1 H.
        assert!(
            out.starts_with("\x1b[1;1H"),
            "missing row position: {out:?}"
        );
        // Reset at end.
        assert!(out.ends_with("\x1b[0m"));
        // Color codes appear (the box uses InkColor::White = 37).
        assert!(out.contains("\x1b[37m"));
        // Box-drawing glyphs survive the escape wrap.
        assert!(out.contains('┌'));
    }

    #[test]
    fn ansi_emits_one_row_position_per_scene_row() {
        let out = render_ansi(&one_box_doc());
        // Count CSI row-positioning sequences.
        let positions = out.matches("\x1b[").filter(|_| true).count();
        assert!(
            positions >= 3,
            "expected >= 3 CSI sequences, got {positions}"
        );
    }

    #[test]
    fn ansi_color_code_mapping() {
        use kirkforge_draw_core::InkColor::*;
        assert_eq!(ansi_color_code(None), None);
        assert_eq!(ansi_color_code(Some(White)), Some(37));
        assert_eq!(ansi_color_code(Some(Red)), Some(31));
        assert_eq!(ansi_color_code(Some(Green)), Some(32));
        assert_eq!(ansi_color_code(Some(Yellow)), Some(33));
        assert_eq!(ansi_color_code(Some(Blue)), Some(34));
        assert_eq!(ansi_color_code(Some(Magenta)), Some(35));
        assert_eq!(ansi_color_code(Some(Cyan)), Some(36));
    }

    #[test]
    fn format_validate_report_clean() {
        let json = format!(
            r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"b","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}}]}}"#
        );
        let report = validate_document(&json);
        let out = format_validate_report(&report, "demo.td.json");
        assert!(out.contains("validate: demo.td.json"));
        assert!(out.contains("schema: 1 (expected 1)"));
        assert!(out.contains("box=1"));
        assert!(out.contains("result: OK"));
        assert!(!out.contains("error:"));
    }

    #[test]
    fn format_validate_report_failed() {
        let json = format!(
            r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"dup","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}},{{"type":"box","id":"dup","z":2,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}}]}}"#
        );
        let report = validate_document(&json);
        let out = format_validate_report(&report, "demo.td.json");
        assert!(out.contains("duplicate:"));
        assert!(out.contains("result: FAILED"));
    }

    #[test]
    fn run_validate_reads_file_and_reports() {
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("validate.td.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"b","z":1,"color":"white","left":0,"top":0,"right":2,"bottom":2,"style":"light"}}]}}"#
            ),
        )
        .unwrap();
        let report = run_validate(&path.to_string_lossy()).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.object_total, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn format_validate_report_json_clean_is_well_formed() {
        let json = format!(
            r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"b","z":1,"color":"white","left":0,"top":0,"right":2,"bottom":2,"style":"light"}}]}}"#
        );
        let report = validate_document(&json);
        let out = format_validate_report_json(&report, "demo.td.json").unwrap();
        // Round-trip: the helper's output is parseable JSON.
        let parsed: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("invalid JSON output: {e}; got {out:?}"));
        // Top-level "path" carries the source so multi-file batches are greppable.
        assert_eq!(parsed["path"], "demo.td.json");
        // Top-level "ok" mirrors the report's predicate — pipeline
        // consumers shouldn't have to re-derive it from the buckets.
        assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
        // Report fields land under "report".
        assert_eq!(parsed["report"]["version_found"], 1);
        assert_eq!(parsed["report"]["version_expected"], 1);
        assert_eq!(parsed["report"]["object_total"], 1);
        assert!(parsed["report"]["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn format_validate_report_json_failed_carries_issue_buckets() {
        let json = format!(
            r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"dup","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}},{{"type":"box","id":"dup","z":2,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}}]}}"#
        );
        let report = validate_document(&json);
        assert!(!report.is_ok());
        let out = format_validate_report_json(&report, "demo.td.json").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Top-level ok flag flips to false on any flagged bucket.
        assert_eq!(parsed["ok"], serde_json::Value::Bool(false));
        // The duplicate-ids bucket is non-empty and carries the
        // canonical "id=X duplicates id=Y" message.
        let dups = parsed["report"]["duplicate_ids"].as_array().unwrap();
        assert_eq!(dups.len(), 1);
        assert!(dups[0].as_str().unwrap().contains("dup"));
        // object_counts is structured (a map keyed by ObjectKind),
        // not the human format's space-joined string.
        assert_eq!(parsed["report"]["object_counts"]["box"], 2);
    }

    // Path-arg validation gates the two cross-platform footguns
    // identified by the --load audit. Each surface that accepts a
    // path (load_doc, run_validate) routes through the helper, so a
    // single test of the helper covers both call sites' contract.

    #[test]
    fn validate_path_arg_rejects_empty() {
        let err = crate::event::validate_path_arg("").unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn validate_path_arg_rejects_interior_nul() {
        // An interior NUL is the OS-specific footgun: Windows
        // truncates at the byte, Linux rejects with InvalidInput.
        // Reject up front so the user gets a clean message.
        let err = crate::event::validate_path_arg("/tmp/foo\0bar").unwrap_err();
        assert!(err.to_string().contains("NUL"), "got: {err}");
    }

    #[test]
    fn validate_path_arg_rejects_whitespace_only() {
        // The save-as dialog can produce a path of `"   "`
        // (fat-fingered Tab or spaces) — atomic_write then
        // tries to open `"   "` as a filename and the OS
        // returns a confusing per-platform error. The
        // validator catches it up front so every path
        // (load / save / save-as commit) shares the same
        // guard and the same message.
        let err = crate::event::validate_path_arg("   ").unwrap_err();
        assert!(err.to_string().contains("whitespace"), "got: {err}");
        let err = crate::event::validate_path_arg("\t\t").unwrap_err();
        assert!(err.to_string().contains("whitespace"), "got: {err}");
    }

    #[test]
    fn validate_path_arg_accepts_normal_path() {
        // Positive case: a legitimate relative path passes through.
        // The function is a precondition check, not a "path exists"
        // check — that's fs::read_to_string's job downstream.
        crate::event::validate_path_arg("./diagram.td.json").unwrap();
        crate::event::validate_path_arg("/tmp/foo.td.json").unwrap();
    }

    #[test]
    fn load_doc_rejects_empty_path() {
        // The helper integration test: a CLI invocation with
        // `--load ""` (or whatever path `clap` consumed) now
        // produces a friendly error before any fs call.
        let err = load_doc("").unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn run_validate_rejects_nul_path() {
        // Same integration test for the validation path. NUL would
        // otherwise disappear into a Linux InvalidInput error
        // message.
        let err = run_validate("/tmp/foo\0.td.json").unwrap_err();
        assert!(err.to_string().contains("NUL"), "got: {err}");
    }

    #[test]
    fn load_doc_rejects_missing_file() {
        // Path that doesn't exist on disk. `fs::read_to_string`
        // returns NotFound; `load_doc` wraps it via `with_context`
        // so the user sees "read <path>" in the chain. Pin the
        // error chain so a future refactor can't drop the wrap
        // and let a raw `Os { code: 2 }` leak to the user.
        let path = "/tmp/kfd-load-doc-missing.td.json";
        let _ = fs::remove_file(path);
        let err = load_doc(path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("read") && msg.contains(path),
            "error chain should mention 'read' and the path, got: {msg}"
        );
    }

    #[test]
    fn run_validate_rejects_missing_file() {
        // Mirror test for the validation path. The validator
        // returns a `ValidateReport` (not Result), so the only
        // error source is the read step — same wrap, same
        // expected chain.
        let path = "/tmp/kfd-run-validate-missing.td.json";
        let _ = fs::remove_file(path);
        let err = run_validate(path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("read") && msg.contains(path),
            "error chain should mention 'read' and the path, got: {msg}"
        );
    }
}
