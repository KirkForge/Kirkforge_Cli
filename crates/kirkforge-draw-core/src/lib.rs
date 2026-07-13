#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! Pure document model and editor state for KirkForge-Draw.
//!
//! This crate is terminal-free. It owns the `.td.json` document model, the
//! geometry / line-glyph helpers, the connection-grid composer, and the
//! editor state machine. The binary crate `kirkforge-draw` adds the TUI
//! and CLI on top.
//!
//! See `docs/adr/0003-document-model.md` for the on-disk format and
//! `docs/adr/0002-crate-layout.md` for the workspace split.

pub mod doc;
pub mod find;
pub mod geometry;
pub mod inspector;
pub mod layers;
pub mod line;
pub mod object;
pub mod palette;
pub mod render_text;
pub mod scene;
pub mod state;
pub mod text_util;
pub mod types;

pub use doc::{
    load_document, new_object_id, round_trip, save_document, validate_document, DocError,
    LoadReport, ObjectKind, ValidateReport,
};
pub use find::{find_matches, MatchField, TextMatch};
pub use geometry::{
    clamp, get_rect_area, get_rect_perimeter_points, is_valid_rect, normalize_rect,
    rect_contains_point,
};
pub use inspector::{format_summary, format_summary_rows, selection_summary, SelectionSummary};
pub use layers::{kind_label, layer_list, layer_row_for_id, LayerEntry};
pub use line::{
    append_paint_segment, constrain_line_point, get_elbow_render_cells,
    get_elbow_render_characters, get_line_points, get_line_render_cells,
    get_line_render_characters, merge_unique_points, point_from_key, points_equal,
};
pub use object::{
    box_handle_contains, box_handle_corner, clone_object_with_id, clone_objects,
    compute_resized_bounds, get_bounds_union, get_box_content_bounds, get_box_corner_points,
    get_line_endpoint_points, get_object_bounds, get_object_render_cells,
    get_object_selection_bounds, hit_test_box_handles, object_contains_point, translate_object,
};
pub use palette::{filter_palette, PaletteAction, PALETTE_ACTIONS};
pub use render_text::{build_scene, render_plain, render_plain_file};
pub use scene::{
    adjust_connection, compose_scene, create_scene, get_box_border_glyph, get_connection_glyph,
    paint_connection_color, stamp_glyph, Scene, SceneCell, CONNECTION_E, CONNECTION_N,
    CONNECTION_S, CONNECTION_W,
};
pub use state::DrawState;
pub use text_util::{
    get_text_content_origin, get_text_render_rect, get_text_selection_bounds,
    normalize_cell_character, pad_to_width, split_graphemes, text_edit_cursor_position,
    truncate_to_cells, visible_cell_count,
};
pub use types::{
    Align, BoxObject, BoxResizeHandle, BoxStyle, DistributeAxis, DrawDocument, DrawMode,
    DrawObject, ElbowObject, ElbowOrientation, InkColor, LineObject, LineStyle, PaintObject, Point,
    Rect, SelectionMode, TextBorderMode, TextObject, DRAW_DOCUMENT_VERSION,
};
