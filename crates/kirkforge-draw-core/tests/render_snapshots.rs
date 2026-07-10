//! Snapshot tests: render each example fixture via the public core API
//! and assert the output matches a checked-in `.txt` file. Catches
//! accidental changes to the glyph table, scene composer, or render
//! path during refactors.
//!
//! To regenerate the snapshots after an intentional render change:
//!   cargo run -q -- --render --load examples/<name>.td.json --plain \
//!     > examples/snapshots/<name>.txt

use kirkforge_draw_core::render_plain_file;

fn workspace_root() -> &'static str {
    // tests/ runs from the crate root (crates/kirkforge-draw-core);
    // the snapshot fixtures live at the workspace root under examples/.
    "../.."
}

fn snapshot_matches(fixture: &str, snapshot: &str) {
    let fixture_path = format!("{root}/examples/{fixture}.td.json", root = workspace_root());
    let snap_path = format!(
        "{root}/examples/snapshots/{snapshot}.txt",
        root = workspace_root()
    );
    let actual = render_plain_file(&fixture_path)
        .unwrap_or_else(|| panic!("render_plain_file failed for {fixture_path}"));
    let expected = std::fs::read_to_string(&snap_path)
        .unwrap_or_else(|e| panic!("read snapshot {snap_path}: {e}"));
    assert_eq!(
        actual, expected,
        "render snapshot for {fixture} drifted — diff expected vs actual:\n--- expected ({snap_path})\n+++ actual\n{}",
        diff_hint(&expected, &actual)
    );
}

fn diff_hint(expected: &str, actual: &str) -> String {
    // ponytail: tiny line-by-line diff instead of pulling in a crate dep
    let exp: Vec<&str> = expected.lines().collect();
    let act: Vec<&str> = actual.lines().collect();
    let max = exp.len().max(act.len());
    let mut out = String::new();
    for i in 0..max {
        let e = exp.get(i).copied().unwrap_or("");
        let a = act.get(i).copied().unwrap_or("");
        if e != a {
            out.push_str(&format!("  L{i:>3} -{e:?}\n"));
            out.push_str(&format!("       +{a:?}\n"));
        }
    }
    out
}

#[test]
fn flowchart_snapshot() {
    snapshot_matches("flowchart", "flowchart");
}

#[test]
fn ui_mock_snapshot() {
    snapshot_matches("ui-mock", "ui-mock");
}

#[test]
fn network_diagram_snapshot() {
    snapshot_matches("network-diagram", "network-diagram");
}

// Negative-path coverage for the `Option` returns. Both
// `build_scene` and `render_plain_file` are exercised heavily on
// the Ok arm by the snapshot tests above; their None arms are
// equally reachable (empty document; unparseable file) and were
// untested. Pin both here so a future refactor of either helper
// can't quietly fail-empty on user input.

#[test]
fn build_scene_returns_none_for_empty_document() {
    use kirkforge_draw_core::{build_scene, DrawDocument};
    let doc = DrawDocument {
        version: 1,
        objects: vec![],
    };
    assert!(
        build_scene(&doc).is_none(),
        "empty document must produce no scene"
    );
}

#[test]
fn render_plain_file_returns_none_for_nonexistent_path() {
    // Path that can't exist on a Linux box: a NUL byte would
    // crash std::fs on most platforms, so use a clearly-synthetic
    // path under /tmp that is not present.
    let path = "/tmp/kfd-render-plain-file-does-not-exist.td.json";
    // Ensure the path really doesn't exist.
    let _ = std::fs::remove_file(path);
    assert!(
        render_plain_file(path).is_none(),
        "missing file must produce None, not panic"
    );
}

#[test]
fn render_plain_file_returns_none_for_unparseable_json() {
    // Write a file that exists but is not valid JSON. The load
    // step inside `render_plain_file` should return None rather
    // than propagating the error.
    let dir = std::env::temp_dir().join("kfd-render-plain-file-tests");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("garbage.td.json");
    std::fs::write(&path, b"this is not json").expect("seed");
    let result = render_plain_file(&path.to_string_lossy());
    assert!(
        result.is_none(),
        "unparseable .td.json must produce None, not panic"
    );
}
