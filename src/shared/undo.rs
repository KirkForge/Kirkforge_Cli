// WO 28.1: UndoKind relocated here from session::undo. It is a pure enum with
// no session state; the disk-backed UndoStack/UndoOp stay in session::undo and
// re-export this type so their callers keep resolving.

/// Kind of edit. Display only — restore is identical for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UndoKind {
    Edit,
    Write,
}

impl UndoKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            UndoKind::Edit => "edit",
            UndoKind::Write => "write",
        }
    }
}
