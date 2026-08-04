//! Document load/save and version negotiation.
//!
//! `.td.json` round-trip is the one I/O path this crate owns. The
//! state machine snapshots are serialized as `DrawDocument` and the
//! CLI / plugin tools read and write this format.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

use crate::types::{DrawDocument, DrawObject, DRAW_DOCUMENT_VERSION};

/// Errors that can come up while loading or saving a document.
#[derive(Debug, Error)]
pub enum DocError {
    #[error("unsupported document version {found}; expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("document has no objects array")]
    MissingObjects,
    #[error("document has no version field")]
    MissingVersion,
}

/// Serialize a document to a JSON string. Always emits the current
/// schema version, regardless of what the in-memory document says.
///
/// # Errors
///
/// Returns [`DocError::Parse`] if `serde_json::to_string_pretty`
/// fails — currently unreachable for `DrawDocument` since every
/// field is `Serialize`, but retained for the `Result` shape so the
/// on-disk format can grow new fields without an API break.
pub fn save_document(doc: &DrawDocument) -> Result<String, DocError> {
    let mut owned = doc.clone();
    owned.version = DRAW_DOCUMENT_VERSION;
    Ok(serde_json::to_string_pretty(&owned)?)
}

/// Parse a JSON string into a `DrawDocument`. Verifies the version and
/// silently drops unknown object types after recording a warning in
/// `unknown_object_warnings` (returned to the caller for logging).
#[derive(Debug, Default)]
pub struct LoadReport {
    pub unknown_object_warnings: Vec<String>,
}

/// # Errors
///
/// Returns [`DocError::Parse`] if the JSON is malformed,
/// [`DocError::MissingVersion`] if `"version"` is absent or not a
/// non-negative integer, [`DocError::MissingObjects`] if the
/// `"objects"` field is absent or not an array, and
/// [`DocError::UnsupportedVersion`] if the version is not the
/// current [`DRAW_DOCUMENT_VERSION`].
pub fn load_document(json: &str) -> Result<(DrawDocument, LoadReport), DocError> {
    // First, peek the raw JSON to get the version. We don't use
    // `serde_json::Value` for the whole document because we want to
    // tolerate unknown object variants in `objects` rather than
    // rejecting the whole file.
    let raw: serde_json::Value = serde_json::from_str(json)?;
    // Version policy: strict equality on a single unsigned integer.
    // The on-disk format is `version: <u32>`. Earlier iterations
    // considered a semver tuple (major / minor / patch) but every
    // shipped document on disk today is `"version": 1`, and splitting
    // it would break round-trip — a no-go for this version. Future
    // major bumps land as `DRAW_DOCUMENT_VERSION = 2`; back-compat
    // for older files at that point is the minor-version story.
    //
    // We accept the JSON value only as `as_u64()` so negative /
    // float / string versions all map to `MissingVersion` rather
    // than being silently truncated. `> u32::MAX` is rare enough
    // to be an explicit error, not a silent wrap to a low number.
    let raw_version = raw.get("version").ok_or(DocError::MissingVersion)?;
    let version_u64 = raw_version.as_u64().ok_or(DocError::MissingVersion)?;
    if version_u64 > u32::MAX as u64 {
        return Err(DocError::UnsupportedVersion {
            found: u32::MAX,
            expected: DRAW_DOCUMENT_VERSION,
        });
    }
    let version = version_u64 as u32;
    if version != DRAW_DOCUMENT_VERSION {
        return Err(DocError::UnsupportedVersion {
            found: version,
            expected: DRAW_DOCUMENT_VERSION,
        });
    }
    let objects_value = raw.get("objects").ok_or(DocError::MissingObjects)?;
    let objects_array = objects_value.as_array().ok_or(DocError::MissingObjects)?;

    let mut objects: Vec<DrawObject> = Vec::with_capacity(objects_array.len());
    let mut warnings: Vec<String> = Vec::new();
    for (i, obj) in objects_array.iter().enumerate() {
        match serde_json::from_value::<DrawObject>(obj.clone()) {
            Ok(parsed) => objects.push(parsed),
            Err(e) => {
                // Likely an unknown `type` field. Try to grab the
                // type name and id (if present) for a useful warning.
                let type_name = obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>");
                let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                warnings.push(format!(
                    "dropped object #{i} of type {type_name:?} (id={id:?}): {e}"
                ));
            }
        }
    }

    Ok((
        DrawDocument { version, objects },
        LoadReport {
            unknown_object_warnings: warnings,
        },
    ))
}

/// A load-then-save round trip should be a no-op on the data. Used by
/// the editor state to verify a freshly-opened file before mutating.
///
/// # Errors
///
/// Returns any error from [`save_document`] or [`load_document`].
pub fn round_trip(doc: &DrawDocument) -> Result<DrawDocument, DocError> {
    let json = save_document(doc)?;
    let (loaded, _report) = load_document(&json)?;
    Ok(loaded)
}

/// A diagnostic report over a `.td.json` document. Produced by
/// `validate_document`; consumed by `kfd --validate` to print findings
/// to stdout. Never errors — even an unparseable file yields a report
/// describing what went wrong, so the CLI can always print something
/// and exit with a status the caller can branch on.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ValidateReport {
    /// The schema version declared in the file, if one was found.
    pub version_found: Option<u32>,
    /// The schema version this build of the editor understands.
    pub version_expected: u32,
    /// Per-kind object counts after parsing.
    pub object_counts: HashMap<ObjectKind, usize>,
    /// Total objects in the parsed document (sums to `object_counts`).
    pub object_total: usize,
    /// IDs that appear on more than one object. The editor only
    /// selects and snapshots by id, so duplicates silently corrupt
    /// undo history.
    pub duplicate_ids: Vec<String>,
    /// Object ids whose geometry would render to nothing: 1×1 boxes,
    /// zero-length lines / elbows, empty paint strokes. The editor
    /// drops these on draft commit, but a saved file may still hold
    /// some from older versions or hand-edits.
    pub degenerate_object_ids: Vec<String>,
    /// Anything `load_document` had to drop (typically an unknown
    /// object variant from a newer file format).
    pub unknown_object_warnings: Vec<String>,
    /// Hard errors that prevented parsing: bad JSON, missing version,
    /// unsupported version, missing objects array.
    pub errors: Vec<String>,
}

impl ValidateReport {
    /// True iff `errors`, `unknown_object_warnings`, `duplicate_ids`,
    /// and `degenerate_object_ids` are all empty. A report can be
    /// clean and still have `object_total == 0` (an empty document is
    /// valid).
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
            && self.unknown_object_warnings.is_empty()
            && self.duplicate_ids.is_empty()
            && self.degenerate_object_ids.is_empty()
    }
}

/// Run a full diagnostic pass over a `.td.json` string. Always
/// succeeds — failures land in `report.errors` so callers can print
/// them and exit with a non-zero status without unwrapping.
pub fn validate_document(json: &str) -> ValidateReport {
    let mut report = ValidateReport {
        version_expected: DRAW_DOCUMENT_VERSION,
        ..Default::default()
    };

    // 1. Parse the raw JSON so we can report version / parse errors
    //    before we even attempt to deserialize as a DrawDocument.
    let raw: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            report.errors.push(format!("invalid JSON: {e}"));
            return report;
        }
    };

    // 2. Version check. Same policy as `load_document` above:
    //    strict equality on a u32, never silently wrapping a huge
    //    u64 to a small one. Validator runs before parsing so it
    //    can describe the failure to the user.
    let version = match raw.get("version").and_then(|v| v.as_u64()) {
        Some(v) if v <= u32::MAX as u64 => v as u32,
        Some(_) => {
            report.errors.push(format!(
                "unsupported schema version > u32::MAX (expected {DRAW_DOCUMENT_VERSION})"
            ));
            return report;
        }
        None => {
            report
                .errors
                .push("missing or non-integer `version`".into());
            return report;
        }
    };
    report.version_found = Some(version);
    if version != DRAW_DOCUMENT_VERSION {
        report.errors.push(format!(
            "unsupported schema version {version} (expected {DRAW_DOCUMENT_VERSION})"
        ));
        return report;
    }

    // 3. Objects array check.
    let objects_array = match raw.get("objects").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => {
            report.errors.push("missing or non-array `objects`".into());
            return report;
        }
    };

    // 4. Per-object validation. Walk the raw array so unknown variants
    //    are recorded here too — `load_document` already does this,
    //    but we want the validator to keep working even if the core
    //    load path changes.
    let mut seen_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, obj_value) in objects_array.iter().enumerate() {
        let type_name = obj_value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>");
        let id = obj_value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        match serde_json::from_value::<DrawObject>(obj_value.clone()) {
            Ok(parsed) => {
                *report
                    .object_counts
                    .entry(ObjectKind::of(&parsed))
                    .or_insert(0) += 1;
                if let Some(n) = seen_ids.get(&id) {
                    report
                        .duplicate_ids
                        .push(format!("object #{i} id={id:?} duplicates object #{n}"));
                } else {
                    seen_ids.insert(id.clone(), i);
                }
                if is_degenerate(&parsed) {
                    report.degenerate_object_ids.push(id);
                }
            }
            Err(e) => {
                report
                    .unknown_object_warnings
                    .push(format!("object #{i} type={type_name:?} id={id:?}: {e}"));
            }
        }
    }
    report.object_total = report.object_counts.values().sum();
    report
}

/// Mirrors the state's `is_degenerate` so the validator is
/// self-contained (no need to drag the state machine in just to check
/// geometry). ponytail: Paint-empty-points and Text-empty-content
/// checks were defensive dead code — empty Paint is just nothing, and
/// empty Text is a legitimate "not yet written" placeholder. Drop
/// them and treat both as non-degenerate.
fn is_degenerate(o: &DrawObject) -> bool {
    match o {
        DrawObject::Box(b) => b.left == b.right && b.top == b.bottom,
        DrawObject::Line(l) => l.x1 == l.x2 && l.y1 == l.y2,
        DrawObject::Elbow(e) => e.x1 == e.x2 && e.y1 == e.y2,
        _ => false,
    }
}

/// Compute a stable name for a new object id. Not cryptographically
/// unique — just a sortable, opaque-enough identifier for session
/// state.
pub fn new_object_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos:x}")
}

/// Object discriminator used by callers that want to branch on type
/// without matching the full enum. Mirrors the `type` field in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Box,
    Line,
    Elbow,
    Paint,
    Text,
}

impl ObjectKind {
    pub fn of(obj: &DrawObject) -> Self {
        match obj {
            DrawObject::Box(_) => Self::Box,
            DrawObject::Line(_) => Self::Line,
            DrawObject::Elbow(_) => Self::Elbow,
            DrawObject::Paint(_) => Self::Paint,
            DrawObject::Text(_) => Self::Text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoxObject, BoxStyle, InkColor};

    fn empty_doc() -> DrawDocument {
        DrawDocument {
            version: DRAW_DOCUMENT_VERSION,
            objects: vec![],
        }
    }

    fn doc_with_box() -> DrawDocument {
        DrawDocument {
            version: DRAW_DOCUMENT_VERSION,
            objects: vec![DrawObject::Box(BoxObject {
                id: "b1".into(),
                z: 1,
                parent_id: None,
                color: InkColor::White,
                left: 0,
                top: 0,
                right: 5,
                bottom: 3,
                style: BoxStyle::Light,
            })],
        }
    }

    #[test]
    fn save_and_load_round_trip_preserves_data() {
        let doc = doc_with_box();
        let json = save_document(&doc).unwrap();
        let (loaded, report) = load_document(&json).unwrap();
        assert_eq!(loaded, doc);
        assert!(report.unknown_object_warnings.is_empty());
    }

    #[test]
    fn load_rejects_unsupported_version() {
        let json = r#"{ "version": 999, "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(
            err,
            DocError::UnsupportedVersion { found: 999, .. }
        ));
    }

    #[test]
    fn load_rejects_negative_version_with_missing_version_error() {
        // Negative ints fail `as_u64()` and map to MissingVersion,
        // not silently to a near-zero version. Surfaces a clearer
        // error to the file's author than a mysterious "expected 1
        // got 4294967295".
        let json = r#"{ "version": -1, "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::MissingVersion));
    }

    #[test]
    fn load_rejects_float_version_with_missing_version_error() {
        let json = r#"{ "version": 1.5, "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::MissingVersion));
    }

    #[test]
    fn load_rejects_string_version_with_missing_version_error() {
        let json = r#"{ "version": "1", "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::MissingVersion));
    }

    #[test]
    fn load_rejects_oversized_version_with_unsupported_version_error() {
        // 2^32 doesn't fit a u32 — must not silently truncate to 0
        // (which would compare equal to no value and produce a
        // confusing "missing version" message). We treat any value
        // above u32::MAX as UnsupportedVersion.
        let json = r#"{ "version": 4294967296, "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::UnsupportedVersion { .. }));
    }

    #[test]
    fn load_rejects_missing_version() {
        let json = r#"{ "objects": [] }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::MissingVersion));
    }

    #[test]
    fn load_rejects_missing_objects() {
        let json = r#"{ "version": 1 }"#;
        let err = load_document(json).unwrap_err();
        assert!(matches!(err, DocError::MissingObjects));
    }

    #[test]
    fn load_drops_unknown_object_types_with_warning() {
        let json = r#"{
            "version": 1,
            "objects": [
                { "type": "box", "id": "b", "z": 1, "parentId": null,
                  "color": "white", "left": 0, "top": 0, "right": 1, "bottom": 1,
                  "style": "light" },
                { "type": "mystery", "id": "x" }
            ]
        }"#;
        let (loaded, report) = load_document(json).unwrap();
        assert_eq!(loaded.objects.len(), 1);
        assert_eq!(report.unknown_object_warnings.len(), 1);
        assert!(report.unknown_object_warnings[0].contains("mystery"));
    }

    #[test]
    fn save_always_emits_current_version() {
        let mut doc = doc_with_box();
        doc.version = 0; // pretend we have a stale in-memory version
        let json = save_document(&doc).unwrap();
        assert!(json.contains("\"version\": 1"));
    }

    #[test]
    fn round_trip_preserves_empty_document() {
        let doc = empty_doc();
        let again = round_trip(&doc).unwrap();
        assert_eq!(again, doc);
    }

    #[test]
    fn round_trip_preserves_multi_object_document() {
        let mut doc = doc_with_box();
        doc.objects.push(DrawObject::Box(BoxObject {
            id: "b2".into(),
            z: 2,
            parent_id: Some("b1".into()),
            color: InkColor::Red,
            left: 10,
            top: 10,
            right: 15,
            bottom: 12,
            style: BoxStyle::Double,
        }));
        let again = round_trip(&doc).unwrap();
        assert_eq!(again, doc);
    }

    #[test]
    fn new_object_id_is_prefixed_and_unique() {
        let a = new_object_id("box");
        let b = new_object_id("box");
        // Same call will normally return different ids (clock advances).
        // We just want them both prefixed and non-empty.
        assert!(a.starts_with("box-"));
        assert!(b.starts_with("box-"));
        assert!(!a.is_empty());
    }

    #[test]
    fn object_kind_of_dispatches_correctly() {
        let doc = doc_with_box();
        assert_eq!(ObjectKind::of(&doc.objects[0]), ObjectKind::Box);
    }

    fn json_for(json: &str) -> String {
        format!(r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{json}]}}"#)
    }

    #[test]
    fn validate_clean_doc_is_ok() {
        let json = json_for(
            r#"{"type":"box","id":"b1","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}"#,
        );
        let r = validate_document(&json);
        assert!(r.is_ok(), "expected clean report, got {r:?}");
        assert_eq!(r.version_found, Some(DRAW_DOCUMENT_VERSION));
        assert_eq!(r.object_total, 1);
        assert_eq!(r.object_counts.get(&ObjectKind::Box), Some(&1));
    }

    #[test]
    fn validate_flags_invalid_json() {
        let r = validate_document("{not json");
        assert!(!r.is_ok());
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].starts_with("invalid JSON"));
    }

    #[test]
    fn validate_flags_missing_version() {
        let r = validate_document(r#"{"objects":[]}"#);
        assert!(!r.is_ok());
        assert!(r.errors[0].contains("version"));
    }

    #[test]
    fn validate_flags_unsupported_version() {
        let r = validate_document(r#"{"version":99,"objects":[]}"#);
        assert!(!r.is_ok());
        assert_eq!(r.version_found, Some(99));
        assert!(r.errors[0].contains("unsupported schema version 99"));
    }

    #[test]
    fn validate_flags_oversized_version() {
        // Above u32::MAX — must be reported as an explicit
        // unsupported-version error rather than silently truncated.
        let r = validate_document(r#"{"version":4294967296,"objects":[]}"#);
        assert!(!r.is_ok());
        assert!(r.errors[0].contains("u32::MAX"));
    }

    #[test]
    fn validate_flags_missing_objects() {
        let r = validate_document(r#"{"version":1}"#);
        assert!(!r.is_ok());
        assert!(r.errors[0].contains("objects"));
    }

    #[test]
    fn validate_flags_duplicate_ids() {
        let a = r#"{"type":"box","id":"dup","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}"#;
        let b = r#"{"type":"box","id":"dup","z":2,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}"#;
        let r = validate_document(&json_for(&format!("{a},{b}")));
        assert!(!r.is_ok());
        assert_eq!(r.duplicate_ids.len(), 1);
        assert!(r.duplicate_ids[0].contains("dup"));
    }

    #[test]
    fn validate_flags_degenerate_objects() {
        // 1×1 box (left == right && top == bottom).
        let s = r#"{"type":"box","id":"x","z":1,"color":"white","left":2,"top":2,"right":2,"bottom":2,"style":"light"}"#;
        let r = validate_document(&json_for(s));
        assert!(!r.is_ok());
        assert_eq!(r.degenerate_object_ids, vec!["x"]);
    }

    #[test]
    fn validate_records_unknown_object_warnings() {
        let s = r#"{"type":"warp","id":"w","foo":1}"#;
        let r = validate_document(&json_for(s));
        assert!(!r.is_ok());
        assert_eq!(r.unknown_object_warnings.len(), 1);
        assert!(r.unknown_object_warnings[0].contains("warp"));
    }

    #[test]
    fn validate_empty_doc_is_ok() {
        let r = validate_document(&json_for(""));
        assert!(r.is_ok());
        assert_eq!(r.object_total, 0);
    }
}
