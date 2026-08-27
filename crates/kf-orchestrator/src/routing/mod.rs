//! `routing` — pure Rust port of the kf-plugin orchestrator's pure
//! modules (folded from the former `kf-routing` crate, WO 47.4).
//! Foundation for the orchestrator port (WO 29.7).
//!
//! Modules:
//! - [`engine`]: FNV-1a vectorizer + cosine similarity + task-family
//!   classifier (port of `memory-palace/src/routing-engine.ts`).
//! - [`classifier`]: regex + TF-IDF task classifier (`classifier.ts` +
//!   `classifier-nlp.ts`).
//! - [`correction`]: correction-loop decision + single-precedence truth
//!   table (`correction-loop.ts` + `truth-model.ts`).
//! - [`profile`]: task-language emission profiles + detection (`task-profile.ts`).
//! - [`cost`]: provider cost rates + estimator (`cost.ts`).
//! - [`path_safety`]: artifact path/content guards + atomic writes
//!   (`path-safety.ts`).
//!
//! No model calls, no event bus, no tokio. The only fs touch is
//! [`path_safety::write_artifacts`] (atomic writes via std::fs).

pub mod classifier;
pub mod correction;
pub mod cost;
pub mod engine;
pub mod path_safety;
pub mod profile;

pub use classifier::{
    classify_hybrid, classify_nlp, classify_task, DelegationDecision, DelegationMode, NlpResult,
    TaskInput, NLP_FALLBACK_THRESHOLD,
};
pub use correction::{
    compute_final_verdict, decide_correction, validation_outcome_for_memory, CorrectionAction,
    CorrectionDecision, FinalAction, FinalVerdict, ReducedStatePacket, SourceOfTruth, TruthInput,
    TruthOutput, ValidationStatus,
};
pub use cost::{estimate_simple_cost, resolve_cost_provider_key};
pub use engine::{
    build_empirical_recommendation, cosine, detect_family, fingerprint_task, normalize_outcome,
    tokenize, vectorize, Fingerprint, Observation, Recommendation, RoutingBias, RoutingCase,
};
pub use path_safety::{
    disallowed_artifact, extract_extension, final_file_is_symlink, has_hidden_segment,
    is_absolute_path, is_binary_like_content, is_inside_cwd, safe_relative_path,
    segments_have_escaping_symlink, sha256_of, sha256_of_raw, write_artifacts, ArtifactRecord,
    TaskProfileLike, WritePolicyLike, WriteResult, MAX_ARTIFACT_BYTES,
};
pub use profile::{
    detect_task_profile, extension_for_language, profile_for_language, EmissionSchema,
    StructuredCheckCommand, TaskLanguage, TaskProfile, WritePolicy,
};
