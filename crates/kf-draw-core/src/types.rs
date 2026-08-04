//! Document model and enums. The shapes here are the on-disk `.td.json`
//! format. See `docs/adr/0003-document-model.md`.

use serde::{Deserialize, Serialize};

/// Current `.td.json` format version. Bump only with a DR.
pub const DRAW_DOCUMENT_VERSION: u32 = 1;

/// Active drawing tool. Matches the termdraw `DrawMode` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrawMode {
    Select,
    Box,
    Line,
    Elbow,
    Paint,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoxStyle {
    Auto,
    Light,
    Heavy,
    Double,
    Dashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineStyle {
    Smooth,
    Light,
    Double,
    Dashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ElbowOrientation {
    HorizontalFirst,
    VerticalFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InkColor {
    White,
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Magenta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextBorderMode {
    None,
    Single,
    Double,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoxResizeHandle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Which edge or center of the union bounds to align the selection to.
/// Used by `DrawState::align_selection` (an editor command, not a
/// document field — no `Serialize`/`Deserialize` derives).
///
/// ponytail: six variants match the Figma / Slack / Miro primitive set;
/// "distribute" (equal spacing) is the next Figma primitive and lives
/// in the sibling enum `DistributeAxis` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Right,
    Top,
    Bottom,
    HorizontalCenter,
    VerticalCenter,
}

/// Which axis to distribute the selection along. Used by
/// `DrawState::distribute_selection` (an editor command, not a
/// document field — no `Serialize`/`Deserialize` derives).
///
/// ponytail: two variants only (Horizontal / Vertical); Figma's
/// "distribute within container" / "distribute by spacing / by center"
/// toggles are out of scope for this tick — endpoints-pinned,
/// equal-spacing is the Slack / Miro default and covers the
/// overwhelming majority of real uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DistributeAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// How `DrawState::select_in_rect` should merge intersecting objects
/// into the existing selection. Mirrors the three modes most editors
/// offer on marquee drag: bare drag replaces, Shift+drag adds,
/// Ctrl+drag toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Drop the existing selection; selection becomes exactly the
    /// objects whose selection-bounds intersect `rect`.
    Replace,
    /// Keep the existing selection; add every intersecting object.
    /// Already-selected objects stay selected (no churn).
    Add,
    /// Flip membership: every intersecting object not currently
    /// selected becomes selected; every intersecting object already
    /// selected becomes unselected.
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxObject {
    pub id: String,
    pub z: i32,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub color: InkColor,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub style: BoxStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineObject {
    pub id: String,
    pub z: i32,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub color: InkColor,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub style: LineStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElbowObject {
    pub id: String,
    pub z: i32,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub color: InkColor,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub style: LineStyle,
    pub orientation: ElbowOrientation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintObject {
    pub id: String,
    pub z: i32,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub color: InkColor,
    pub points: Vec<Point>,
    pub brush: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextObject {
    pub id: String,
    pub z: i32,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub color: InkColor,
    pub x: i32,
    pub y: i32,
    pub content: String,
    pub border: TextBorderMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DrawObject {
    Box(BoxObject),
    Line(LineObject),
    Elbow(ElbowObject),
    Paint(PaintObject),
    Text(TextObject),
}

impl DrawObject {
    pub fn id(&self) -> &str {
        match self {
            DrawObject::Box(o) => &o.id,
            DrawObject::Line(o) => &o.id,
            DrawObject::Elbow(o) => &o.id,
            DrawObject::Paint(o) => &o.id,
            DrawObject::Text(o) => &o.id,
        }
    }

    pub fn z(&self) -> i32 {
        match self {
            DrawObject::Box(o) => o.z,
            DrawObject::Line(o) => o.z,
            DrawObject::Elbow(o) => o.z,
            DrawObject::Paint(o) => o.z,
            DrawObject::Text(o) => o.z,
        }
    }

    pub fn color(&self) -> InkColor {
        match self {
            DrawObject::Box(o) => o.color,
            DrawObject::Line(o) => o.color,
            DrawObject::Elbow(o) => o.color,
            DrawObject::Paint(o) => o.color,
            DrawObject::Text(o) => o.color,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            DrawObject::Box(o) => o.parent_id.as_deref(),
            DrawObject::Line(o) => o.parent_id.as_deref(),
            DrawObject::Elbow(o) => o.parent_id.as_deref(),
            DrawObject::Paint(o) => o.parent_id.as_deref(),
            DrawObject::Text(o) => o.parent_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawDocument {
    pub version: u32,
    pub objects: Vec<DrawObject>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_round_trips() {
        let doc = DrawDocument {
            version: DRAW_DOCUMENT_VERSION,
            objects: vec![],
        };
        let s = serde_json::to_string(&doc).unwrap();
        let back: DrawDocument = serde_json::from_str(&s).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn ink_color_serializes_lowercase() {
        let s = serde_json::to_string(&InkColor::Magenta).unwrap();
        assert_eq!(s, "\"magenta\"");
    }

    #[test]
    fn elbow_orientation_serializes_kebab() {
        let s = serde_json::to_string(&ElbowOrientation::VerticalFirst).unwrap();
        assert_eq!(s, "\"vertical-first\"");
    }

    #[test]
    fn box_object_uses_camel_case_keys() {
        let obj = BoxObject {
            id: "obj-1".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 5,
            bottom: 3,
            style: BoxStyle::Light,
        };
        let v: serde_json::Value = serde_json::to_value(&obj).unwrap();
        assert_eq!(v["parentId"], serde_json::Value::Null);
        assert!(v.get("parent_id").is_none());
        assert_eq!(v["style"], "light");
    }

    #[test]
    fn draw_object_dispatches_by_type() {
        let obj = DrawObject::Line(LineObject {
            id: "obj-2".into(),
            z: 2,
            parent_id: None,
            color: InkColor::Cyan,
            x1: 0,
            y1: 0,
            x2: 3,
            y2: 0,
            style: LineStyle::Smooth,
        });
        let s = serde_json::to_string(&obj).unwrap();
        assert!(s.contains("\"type\":\"line\""), "got: {s}");
        assert_eq!(obj.id(), "obj-2");
        assert_eq!(obj.z(), 2);
    }

    #[test]
    fn color_accessor_reads_inner_variant_color() {
        let obj = DrawObject::Box(BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
            style: BoxStyle::Light,
        });
        assert_eq!(obj.color(), InkColor::Red);
    }

    #[test]
    fn parent_id_accessor_returns_some_or_none() {
        let child = DrawObject::Text(TextObject {
            id: "t".into(),
            z: 1,
            parent_id: Some("box".into()),
            color: InkColor::White,
            x: 0,
            y: 0,
            content: "hi".into(),
            border: TextBorderMode::None,
        });
        assert_eq!(child.parent_id(), Some("box"));

        let standalone = DrawObject::Line(LineObject {
            id: "l".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x1: 0,
            y1: 0,
            x2: 1,
            y2: 1,
            style: LineStyle::Smooth,
        });
        assert_eq!(standalone.parent_id(), None);
    }
}
