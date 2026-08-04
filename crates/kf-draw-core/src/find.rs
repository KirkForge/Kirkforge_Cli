//! Find / find-next by id substring or text content.
//!
//! The pure half of the find feature. `find_matches` walks the
//! document once and returns every occurrence of `query` in
//! either an object's `id` or — for `Text` — its `content`. The
//! bin keeps the "current match" cursor and the inline prompt.
//!
//! ponytail: substring, not regex; case-insensitive, not
//! whole-word; one pass, not a precomputed index. Figma /
//! Slack's "Ctrl-F" convention is substring + case-insensitive
//! and 99% of user queries are < 5 chars in a document of < 500
//! objects — `O(n * |query|)` is fine. Add an index if/when a
//! 10k-object document makes a re-scan noticeable.

use crate::doc::ObjectKind;
use crate::state::DrawState;
use crate::types::DrawObject;

/// What `find_matches` matched on a single object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    /// Object id. Multiple matches on the same object (e.g. id
    /// contains "foo" AND content contains "foo") produce two
    /// entries with the same id but different `field` — the
    /// bin's "next match" cursor advances per entry, so a
    /// user-typed `f-o-o` lands on each field in turn.
    pub id: String,
    /// Object discriminator. Cheap to copy; the bin uses it for
    /// the status echo ("matched Text 'hello'") and to dedupe
    /// rows when projecting onto the layers panel.
    pub kind: ObjectKind,
    /// Which field the query hit.
    pub field: MatchField,
    /// Byte offset into the matched field's lowercased
    /// representation. `None` for `MatchField::Id` (the whole id
    /// is the match; no useful offset). For `MatchField::Content`
    /// this is the position the bin can use to highlight the
    /// hit inside a future inspector field.
    pub offset: Option<usize>,
}

/// Which document field a `TextMatch` hit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchField {
    /// Object id.
    Id,
    /// `TextObject::content` (Text variants only).
    Content,
}

/// Walk every object in document order and return every
/// case-insensitive substring hit of `query` in an id or
/// (for `Text`) in `content`. Empty / whitespace-only queries
/// return an empty `Vec` — calling find with no query is
/// treated as a no-op, not "match every object".
///
/// ponytail: re-`to_lowercase`'s both sides per match instead
/// of pre-lowercasing the document. The query is short and
/// the document is small, so the `O(n)` per-query scan is
/// cheaper than carrying a parallel lowercased index. When
/// find shows up in a hot path we'll revisit.
pub fn find_matches(state: &DrawState, query: &str) -> Vec<TextMatch> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for obj in &state.document.objects {
        let id = obj.id();
        let kind = ObjectKind::of(obj);
        if id.to_lowercase().contains(&q) {
            out.push(TextMatch {
                id: id.to_string(),
                kind,
                field: MatchField::Id,
                offset: None,
            });
        }
        if let DrawObject::Text(t) = obj {
            if let Some(off) = t.content.to_lowercase().find(&q) {
                out.push(TextMatch {
                    id: id.to_string(),
                    kind,
                    field: MatchField::Content,
                    offset: Some(off),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        BoxObject, BoxStyle, DrawObject, InkColor, LineObject, LineStyle, TextBorderMode,
        TextObject,
    };

    fn empty() -> DrawState {
        DrawState::new()
    }

    fn one_box(id: &str) -> DrawState {
        let mut s = DrawState::new();
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: id.into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: BoxStyle::Light,
        }));
        s
    }

    fn one_text(id: &str, content: &str) -> DrawState {
        let mut s = DrawState::new();
        s.document.objects.push(DrawObject::Text(TextObject {
            id: id.into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 0,
            content: content.into(),
            border: TextBorderMode::None,
        }));
        s
    }

    #[test]
    fn empty_query_returns_no_matches() {
        let s = one_box("foo");
        assert!(find_matches(&s, "").is_empty());
        assert!(find_matches(&s, "   ").is_empty());
    }

    #[test]
    fn empty_document_returns_no_matches() {
        let s = empty();
        assert!(find_matches(&s, "foo").is_empty());
    }

    #[test]
    fn id_substring_match_reports_kind_and_no_offset() {
        let s = one_box("alpha-bravo-charlie");
        let m = find_matches(&s, "bravo");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "alpha-bravo-charlie");
        assert_eq!(m[0].kind, ObjectKind::Box);
        assert_eq!(m[0].field, MatchField::Id);
        assert_eq!(m[0].offset, None);
    }

    #[test]
    fn text_content_match_reports_offset() {
        let s = one_text("t1", "hello world");
        let m = find_matches(&s, "world");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "t1");
        assert_eq!(m[0].kind, ObjectKind::Text);
        assert_eq!(m[0].field, MatchField::Content);
        // "hello " is 6 bytes; "world" starts at 6.
        assert_eq!(m[0].offset, Some(6));
    }

    #[test]
    fn non_text_objects_skip_content_match() {
        // A Line has no `content` field; querying for a
        // substring must only match against `id`.
        let mut s = empty();
        s.document.objects.push(DrawObject::Line(LineObject {
            id: "l1".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 4,
            style: LineStyle::Light,
        }));
        let m = find_matches(&s, "l1");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].field, MatchField::Id);
    }

    #[test]
    fn query_is_case_insensitive() {
        let s = one_text("t", "Hello World");
        assert_eq!(find_matches(&s, "WORLD").len(), 1);
        assert_eq!(find_matches(&s, "world").len(), 1);
        assert_eq!(find_matches(&s, "WoRlD").len(), 1);
    }

    #[test]
    fn text_matching_both_id_and_content_emits_two_entries() {
        // A Text whose id is "foo" and whose content is
        // "foo bar" — the query "foo" should produce two
        // matches: one on id, one on content. The bin's
        // "next match" cursor advances per entry, so the
        // user lands on each in turn.
        let s = one_text("foo", "foo bar");
        let m = find_matches(&s, "foo");
        assert_eq!(m.len(), 2);
        let fields: Vec<MatchField> = m.iter().map(|x| x.field).collect();
        assert!(fields.contains(&MatchField::Id));
        assert!(fields.contains(&MatchField::Content));
    }

    #[test]
    fn multiple_objects_each_match_once_in_id() {
        let mut s = empty();
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "alpha".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "beta".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 2,
            top: 0,
            right: 3,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        s.document.objects.push(DrawObject::Text(TextObject {
            id: "gamma".into(),
            z: 2,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 2,
            content: "alpha inside".into(),
            border: TextBorderMode::None,
        }));
        // "alpha" matches: the box whose id is "alpha", and
        // the Text whose content is "alpha inside" (the Text's
        // own id is "gamma" so it doesn't match in id).
        let m = find_matches(&s, "alpha");
        assert_eq!(m.len(), 2, "got {m:?}");
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "gamma"]);
    }

    #[test]
    fn leading_and_trailing_whitespace_in_query_is_trimmed() {
        // Query "  foo  " should match the same as "foo".
        // The trim guards against accidental space-padded
        // clipboard pastes.
        let s = one_box("foobar");
        assert_eq!(find_matches(&s, "  foo  ").len(), 1);
    }

    #[test]
    fn unmatched_query_returns_empty() {
        let s = one_text("t", "hello");
        assert!(find_matches(&s, "xyzzy").is_empty());
    }
}
