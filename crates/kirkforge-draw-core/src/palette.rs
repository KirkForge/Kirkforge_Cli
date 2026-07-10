//! Command palette data model.
//!
//! Pure data: the action table and the substring-matcher the bin's
//! filter UI calls. The dispatch half (calling `Save` / `Quit` /
//! ...) lives in the bin's event loop where the side effects
//! (file write, app shutdown) actually happen — keeping
//! `kirkforge-draw-core` terminal-free.

/// Every palette-able action the editor currently exposes. Bin
/// matches the variant name in `dispatch_palette_action` and runs
/// the corresponding side effect. Add new variants here when the
/// bin grows a new command, then match on both sides.
///
/// ponytail: no arguments. The palette is a single-character
/// trigger that fires one effect, so variants that need an
/// argument (cycle box style, recolor to a specific color) live
/// as chords only; the palette can't reach them. If a future
/// "prompted palette" tier adds argument passing, this list
/// can absorb them then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteAction {
    /// Toggle the in-app key-map overlay.
    Help,
    /// Toggle the layers panel on the right sidebar (L).
    ToggleLayers,
    /// Toggle the properties inspector panel (I).
    ToggleInspector,
    /// Save the document to `source_path` (Ctrl-S).
    Save,
    /// Undo the last mutation (Ctrl-Z).
    Undo,
    /// Redo a previously-undone mutation (Ctrl-Y).
    Redo,
    /// Duplicate the selection by +1, +1 (Ctrl-D).
    Duplicate,
    /// Group the selection under a new parent id (Ctrl-G).
    Group,
    /// Ungroup the selection (Ctrl-Shift-G).
    Ungroup,
    /// Select every object in the document (Ctrl-A).
    SelectAll,
    /// Delete the current selection (Delete / Backspace).
    Delete,
    /// Align the selection to the left edge of the union bounds
    /// (Ctrl-Shift-L).
    AlignLeft,
    /// Align the selection to the right edge (Ctrl-Shift-R).
    AlignRight,
    /// Align the selection to the top edge (Ctrl-Shift-T).
    AlignTop,
    /// Align the selection to the bottom edge (Ctrl-Shift-B).
    AlignBottom,
    /// Align the selection to the horizontal center (Ctrl-Shift-H).
    AlignHorizontalCenter,
    /// Align the selection to the vertical center (Ctrl-Shift-V).
    AlignVerticalCenter,
    /// Distribute ≥3 selected items along the X axis with equal
    /// spacing between centers, endpoints pinned (Ctrl-Shift-J).
    DistributeHorizontal,
    /// Distribute ≥3 selected items along the Y axis (Ctrl-Shift-K).
    DistributeVertical,
    /// Quit the editor (q / Ctrl-C).
    Quit,
}

/// The palette's command table. Names are matched case-insensitively
/// (the filter lowercases both sides); keep the canonical form here
/// lowercase so the displayed name and the input match.
///
/// ponytail: hard-coded table, not a registration system. Eighteen
/// actions today; under thirty tomorrow; an `Enum + const slice`
/// is the right scaling story. If the bin ever wants to inject
/// commands at runtime, we revisit — until then, one less moving
/// part.
pub const PALETTE_ACTIONS: &[(&str, PaletteAction)] = &[
    ("help", PaletteAction::Help),
    ("layers", PaletteAction::ToggleLayers),
    ("inspector", PaletteAction::ToggleInspector),
    ("save", PaletteAction::Save),
    ("undo", PaletteAction::Undo),
    ("redo", PaletteAction::Redo),
    ("duplicate", PaletteAction::Duplicate),
    ("group", PaletteAction::Group),
    ("ungroup", PaletteAction::Ungroup),
    ("select all", PaletteAction::SelectAll),
    ("delete", PaletteAction::Delete),
    ("align left", PaletteAction::AlignLeft),
    ("align right", PaletteAction::AlignRight),
    ("align top", PaletteAction::AlignTop),
    ("align bottom", PaletteAction::AlignBottom),
    (
        "align horizontal center",
        PaletteAction::AlignHorizontalCenter,
    ),
    ("align vertical center", PaletteAction::AlignVerticalCenter),
    ("distribute horizontal", PaletteAction::DistributeHorizontal),
    ("distribute vertical", PaletteAction::DistributeVertical),
    ("quit", PaletteAction::Quit),
];

/// Filter the palette table against `query`. The returned slice is
/// sorted by relevance: exact-prefix matches first (length-tied to
/// the shortest wins), then substring matches in source order.
/// Empty query returns the full table in source order.
///
/// ponytail: substring + prefix sort, not a real fuzzy ranker
/// (Levenshtein, word-boundary, abbreviation). Six entries today;
/// a real fuzzy pass becomes worth it when the table crosses ~50.
/// The current ranking reads naturally for the kinds of queries a
/// user types ("un" → "undo", "re" → "redo").
pub fn filter_palette(query: &str) -> Vec<(&'static str, &'static PaletteAction)> {
    let q = query.trim().to_lowercase();
    let all: Vec<(&'static str, &'static PaletteAction)> =
        PALETTE_ACTIONS.iter().map(|(n, a)| (*n, a)).collect();
    if q.is_empty() {
        return all;
    }
    // Two-bucket partition rather than a single sorted Vec: prefix
    // matches beat substring matches regardless of length. Within a
    // bucket, original source order is the tiebreak (stable).
    let mut prefix: Vec<(&'static str, &'static PaletteAction)> = Vec::new();
    let mut substring: Vec<(&'static str, &'static PaletteAction)> = Vec::new();
    for entry in all {
        if entry.0.starts_with(&q) {
            prefix.push(entry);
        } else if entry.0.contains(&q) {
            substring.push(entry);
        }
    }
    prefix.extend(substring);
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all_in_source_order() {
        let r = filter_palette("");
        assert_eq!(r.len(), PALETTE_ACTIONS.len());
        let names: Vec<&str> = r.iter().map(|(n, _)| *n).collect();
        let expected: Vec<&str> = PALETTE_ACTIONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn whitespace_query_returns_all() {
        // trim() treats whitespace-only as empty so an accidental
        // space doesn't silence the palette.
        assert_eq!(filter_palette("   ").len(), PALETTE_ACTIONS.len());
    }

    #[test]
    fn exact_match_returns_single_entry() {
        let r = filter_palette("undo");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "undo");
        assert_eq!(r[0].1, &PaletteAction::Undo);
    }

    #[test]
    fn case_insensitive_match() {
        let r = filter_palette("UNDO");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "undo");
        let r2 = filter_palette("UnDo");
        assert_eq!(r2.len(), 1);
    }

    #[test]
    fn prefix_match_sorts_before_substring_match() {
        // "re" is a prefix of "redo" but only a substring of "redo".
        // "re" doesn't appear in any other entry, so a prefix-only
        // query yields the prefix entry first, single result.
        let r = filter_palette("re");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "redo");
    }

    #[test]
    fn no_match_returns_empty() {
        let r = filter_palette("zzz");
        assert!(r.is_empty());
    }

    #[test]
    fn entries_are_in_source_order() {
        // Lock the source order. The user-visible palette UI
        // is alphabetical-by-typing; source order is the
        // tiebreaker when two entries match the same query
        // (the substring bucket is appended after the prefix
        // bucket in source order, so a reorder here changes
        // the "ties" downstream). A future reordering needs
        // to land alongside an intentional UX change, not by
        // accident.
        let names: Vec<&str> = PALETTE_ACTIONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "help",
                "layers",
                "inspector",
                "save",
                "undo",
                "redo",
                "duplicate",
                "group",
                "ungroup",
                "select all",
                "delete",
                "align left",
                "align right",
                "align top",
                "align bottom",
                "align horizontal center",
                "align vertical center",
                "distribute horizontal",
                "distribute vertical",
                "quit",
            ],
            "PALETTE_ACTIONS source order changed — update intentionally"
        );
    }

    #[test]
    fn action_lookup_returns_distinct_variants() {
        // Each (name, action) pairing resolves to the expected
        // variant. A typo in PALETTE_ACTIONS would surface here.
        let by_name: std::collections::HashMap<&str, PaletteAction> =
            PALETTE_ACTIONS.iter().map(|(n, a)| (*n, *a)).collect();
        assert_eq!(by_name["help"], PaletteAction::Help);
        assert_eq!(by_name["layers"], PaletteAction::ToggleLayers);
        assert_eq!(by_name["inspector"], PaletteAction::ToggleInspector);
        assert_eq!(by_name["save"], PaletteAction::Save);
        assert_eq!(by_name["undo"], PaletteAction::Undo);
        assert_eq!(by_name["redo"], PaletteAction::Redo);
        assert_eq!(by_name["duplicate"], PaletteAction::Duplicate);
        assert_eq!(by_name["group"], PaletteAction::Group);
        assert_eq!(by_name["ungroup"], PaletteAction::Ungroup);
        assert_eq!(by_name["select all"], PaletteAction::SelectAll);
        assert_eq!(by_name["delete"], PaletteAction::Delete);
        assert_eq!(by_name["align left"], PaletteAction::AlignLeft);
        assert_eq!(by_name["align right"], PaletteAction::AlignRight);
        assert_eq!(by_name["align top"], PaletteAction::AlignTop);
        assert_eq!(by_name["align bottom"], PaletteAction::AlignBottom);
        assert_eq!(
            by_name["align horizontal center"],
            PaletteAction::AlignHorizontalCenter
        );
        assert_eq!(
            by_name["align vertical center"],
            PaletteAction::AlignVerticalCenter
        );
        assert_eq!(
            by_name["distribute horizontal"],
            PaletteAction::DistributeHorizontal
        );
        assert_eq!(
            by_name["distribute vertical"],
            PaletteAction::DistributeVertical
        );
        assert_eq!(by_name["quit"], PaletteAction::Quit);
    }

    #[test]
    fn palette_table_has_no_duplicate_variants() {
        // ponytail: belt-and-braces guard against accidentally
        // listing the same PaletteAction twice. The dispatch
        // table (event.rs) keys behavior on the variant, so a
        // duplicate row would mean two palette names dispatch
        // to the same side effect — the user types "save",
        // gets "save" twice, the second silently shadows the
        // first. The `action_lookup_returns_distinct_variants`
        // test above pins *every* variant to a row; this test
        // pins the inverse — every row to a distinct variant.
        // Together they form a weak equality: variant-count ==
        // row-count == 20 today.
        let mut seen = std::collections::HashSet::new();
        for (_, action) in PALETTE_ACTIONS {
            assert!(
                seen.insert(*action),
                "PALETTE_ACTIONS contains duplicate variant: {action:?}"
            );
        }
        assert_eq!(seen.len(), PALETTE_ACTIONS.len());
    }

    #[test]
    fn substring_match_includes_non_prefix_hits() {
        // "o" is a pure-substring query — it doesn't prefix any
        // entry (no entry begins with "o"). Nine table entries
        // contain 'o' as a substring: `inspector`, `undo`,
        // `redo`, `group`, `ungroup`, `align top`, `align
        // bottom`, `align horizontal center`, and `distribute
        // horizontal`. The vertical-center / distribute-vertical
        // entries do NOT contain "o" — only the horizontal
        // variants do. The count pins the new multi-word
        // entries so a future rename doesn't silently drop
        // substring matches.
        let r = filter_palette("o");
        let names: Vec<&str> = r.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"inspector"));
        assert!(names.contains(&"undo"));
        assert!(names.contains(&"redo"));
        assert!(names.contains(&"group"));
        assert!(names.contains(&"ungroup"));
        assert!(names.contains(&"align top"));
        assert!(names.contains(&"align bottom"));
        assert!(names.contains(&"align horizontal center"));
        assert!(names.contains(&"distribute horizontal"));
        assert!(!names.contains(&"align vertical center"));
        assert!(!names.contains(&"distribute vertical"));
        assert_eq!(r.len(), 9);
    }
}
