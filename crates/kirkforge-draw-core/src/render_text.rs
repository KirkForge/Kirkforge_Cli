//! Pure text-rendering helpers (no TUI deps).
//!
//! Used by the `kfd --render --plain` path and the snapshot tests
//! under `tests/`. ANSI and fenced-markdown flavors stay in the bin
//! crate because they carry terminal-specific formatting concerns.

use crate::{
    compose_scene as compose_core, create_scene, load_document, object::get_object_bounds,
    DrawDocument, Point, Rect, Scene,
};

/// Build the scene for a document, sized to enclose every object's
/// bounds with a 1-cell margin. Returns `None` for an empty document.
pub fn build_scene(doc: &DrawDocument) -> Option<Scene> {
    let bounds: Option<Rect> =
        doc.objects
            .iter()
            .filter_map(get_object_bounds)
            .fold(None, |acc, r| {
                Some(match acc {
                    None => r,
                    Some(prev) => Rect {
                        left: prev.left.min(r.left),
                        top: prev.top.min(r.top),
                        right: prev.right.max(r.right),
                        bottom: prev.bottom.max(r.bottom),
                    },
                })
            });
    let r = bounds?;
    let width = r.right - r.left + 1 + 2;
    let height = r.bottom - r.top + 1 + 2;
    let origin = Point {
        x: r.left - 1,
        y: r.top - 1,
    };
    let mut scene = create_scene(width, height, origin);
    compose_core(&mut scene, &doc.objects);
    Some(scene)
}

/// Render a document to a plain-text string. Trailing spaces per line
/// are trimmed so the output is paste-friendly.
pub fn render_plain(doc: &DrawDocument) -> String {
    let scene = match build_scene(doc) {
        Some(s) => s,
        None => return String::from("\n"),
    };
    let mut out = String::new();
    for row in &scene.cells {
        let line: String = row.iter().map(|c| c.glyph).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Load and render a `.td.json` file in one step. Convenience for CLI
/// callers and tests; returns `None` only for an unparseable file
/// (caller's choice how to surface).
pub fn render_plain_file(path: &str) -> Option<String> {
    let json = std::fs::read_to_string(path).ok()?;
    let (doc, _report) = load_document(&json).ok()?;
    Some(render_plain(&doc))
}
