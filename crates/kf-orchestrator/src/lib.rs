//! `kf-orchestrator` — Rust port of `@kirkforge/orchestrator` (WO 29.7).
//!
//! Brings together the delegation + decompose + correction pipeline that
//! the TS orchestrator (7955 LOC, 36 files) implemented. The pure decision
//! logic (classifier, routing, correction, truth model, profiles, cost,
//! path safety) lives in `kf-routing`; this crate owns the stateful
//! orchestration that ties those decisions to model calls + memory writes.
//!
//! ## Wiring status (WO 35.6)
//! `ModelClient` has a production implementation in the kf-code binary:
//! `session::executor_adapter::ExecutorAdapter` maps a [`model::TaskBrief`]
//! onto an isolated subagent session (ADR-075 flattening: final assistant
//! message + summed usage). Tests use [`model::RecordingClient`].
//! The deterministic reducer + verifier bus (`orchestrator-verifiers.ts`,
//! `reducer.ts`) is still NOT ported here; the packet on each
//! `DelegationResult` is `None` until that ships. The correction loop
//! still functions: it feeds the (possibly-default) packet into
//! `kf_routing::correction::decide_correction`.

pub mod correction;
pub mod correction_loop_helpers;
pub mod decompose;
pub mod delegate;
pub mod model;
pub mod modes;
pub mod sink;
pub mod types;
pub mod verifier;
pub mod workspace;

pub use correction::{run_correction_loop, LoopDelegate};
pub use correction_loop_helpers::task_outcome_from_validation;
pub use decompose::{
    decompose_task, execute_decomposition, parse_decomposition, topological_sort, DelegateFn,
    MAX_SUBTASKS,
};
pub use delegate::{Orchestrator, OrchestratorConfig};
pub use model::{BriefCancel, ModelClient, PanickingClient, RecordingClient, TaskBrief};
pub use modes::{
    execute_artifact, execute_hard_prompt, execute_schema_contract, finalize_artifact,
    finalize_hard_prompt, finalize_schema_contract, flush_signals_to_sink, parse_artifacts,
    parse_jsonl_artifacts, persist_code_blocks, ParseResult, ParsedArtifact, PersistOutcome,
};
pub use sink::{ArtifactEvent, EventSink, NullSink, RecordingSink};
pub use types::{
    extract_emission_files, extract_written_files, CorrectionLoopConfig, CorrectionLoopOutcome,
    DecompositionExecutionResult, DecompositionResult, DelegationDecisionInfo, DelegationResult,
    Emission, EmittedFileInfo, OrchestratorStats, Signal, SubtaskExecutionResult, TaskInput,
    TaskNode, TaskValidationResult, ValidatorConfig,
};
pub use verifier::{apply_security_findings, scan_files, SecurityFinding};
pub use workspace::{IsolatedWorkspace, OverlaySpec, WorkspaceManager};

// Re-export the upstream kf-routing pieces the orchestrator surface
// implies so consumers can `use kf_orchestrator::DelegationMode`.
pub use kf_routing::classifier::DelegationMode;
pub use kf_routing::profile::TaskProfile;
