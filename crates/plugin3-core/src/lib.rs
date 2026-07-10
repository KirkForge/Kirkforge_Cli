//! plugin3-core — pure logic for tool output slicing, token budgeting,
//! offload storage, and usage emission. No host I/O.

pub mod atomic_write;
pub mod budget;
pub mod compaction;
pub mod cost;
pub mod detector;
pub mod error;
pub mod orchestrator;
pub mod paths;
pub mod report;
pub mod slicing;
pub mod store;
pub mod text;

pub mod test_support;

pub use atomic_write::atomic_write_text;
pub use budget::{
    decide, estimate_tokens, BudgetConfig, BudgetState, ConfigFile, Intervention, TokenBudget,
    UsageConfig,
};
pub use compaction::{
    build_hint, local_summarise, CompactHint, CompactedOutput, CompactionTransform,
    LocalSummaryCompactor, Turn,
};
pub use cost::{classify_kind, emit_usage, is_usage_enabled_at, UsageKind, UsageRecord};
pub use detector::{detect, should_slice, Decision, ToolOutputKind};
pub use error::TransformError;
pub use orchestrator::{
    run as run_orchestrator, DetectorCache, OrchestratorResult, SliceDecision, SlicingOrchestrator,
};
pub use paths::Paths;
pub use report::{
    aggregate_sessions, filter_lines, format_summary_line, tail_lines, SessionTotals,
};
pub use slicing::{slice_or_skip, HeadTailSlicer, SlicedOutput, SlicingTransform};
pub use store::{
    format_slice_marker, make_key, parse_slice_marker, validate_key, FileOffloadStore,
    InMemoryOffloadStore, OffloadStore, StoreError, SLICE_MARKER_PREFIX, SLICE_MARKER_SUFFIX,
};
