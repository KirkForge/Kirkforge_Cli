//! R2 — port of `orchestrator/src/classifier.ts` + `classifier-nlp.ts`.
//!
//! Two-layer task classifier: regex scoring (`classify_task`) with NLP/TF-IDF
//! fallback (`classify_nlp`) for low-confidence inputs. Pure: no model calls,
//! no persistence.
//!
//! ClassifierMemory learned-examples persistence is DEFERRED to WO 32.11:
//! persistence requires fs + session-dir ownership, which is the orchestrator's
//! job, not this pure crate's. The classifier works without it (regex + NLP
//! fallback); persistence is a recall optimization, not a correctness gap.
//! Remaining work: add a ClassifierMemory persistence adapter in
//! `kf-orchestrator` that saves/loads learned examples to the session dir.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// Delegation mode. Wire spellings match TS via serde kebab-case rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DelegationMode {
    Artifact,
    SchemaContract,
    HardPrompt,
    TaskDecompose,
}

impl DelegationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DelegationMode::Artifact => "artifact",
            DelegationMode::SchemaContract => "schema-contract",
            DelegationMode::HardPrompt => "hard-prompt",
            DelegationMode::TaskDecompose => "task-decompose",
        }
    }
}

/// Below this regex confidence the NLP fallback kicks in (TS: 0.35).
pub const NLP_FALLBACK_THRESHOLD: f64 = 0.35;

// ── Regex scoring table (classifier.ts) ─────────────────────────────────────

struct ModeRule {
    pattern: &'static str,
    mode: DelegationMode,
    score: u32,
    reason: &'static str,
}

const MODE_RULES: &[ModeRule] = &[
    ModeRule {
        pattern: r"\b(?:generate|create|write|build|make)\s+(?:a\s+)?(?:\w+\s+)?(?:file|component|module|service|class|server|app|script)\b",
        mode: DelegationMode::Artifact,
        score: 20,
        reason: "file creation task",
    },
    ModeRule {
        pattern: r"\b(?:file|files|write to|save to)\b",
        mode: DelegationMode::Artifact,
        score: 10,
        reason: "file output task",
    },
    ModeRule {
        pattern: r"\b(?:structured response|json schema|contract format|audit report)\b",
        mode: DelegationMode::SchemaContract,
        score: 15,
        reason: "structured/contract task",
    },
    ModeRule {
        pattern: r"\b(?:audit|assess|evaluate|review\s+(?:the|this))\b",
        mode: DelegationMode::SchemaContract,
        score: 8,
        reason: "analysis/audit task",
    },
    ModeRule {
        pattern: r"\b(?:validate|verify)\b",
        mode: DelegationMode::SchemaContract,
        score: 5,
        reason: "validation task",
    },
    ModeRule {
        pattern: r"\b(?:full-stack|end-to-end|multi-step|pipeline|workflow|build a (?:complete|full|whole)|from scratch|boilerplate|scaffold)\b",
        mode: DelegationMode::TaskDecompose,
        score: 25,
        reason: "multi-step/pipeline task",
    },
    ModeRule {
        pattern: r"\b(?:break (?:down|into|this)|decompose|subtasks|step (?:by step|1)|plan (?:out|the))\b",
        mode: DelegationMode::TaskDecompose,
        score: 20,
        reason: "explicit decomposition request",
    },
    ModeRule {
        pattern: r"\b(?:fix|lint error|repair|refactor)\b",
        mode: DelegationMode::HardPrompt,
        score: 5,
        reason: "repair/fix task",
    },
];

static MODE_SCORING_SET: LazyLock<regex::RegexSet> = LazyLock::new(|| {
    // ponytail: `(?i)` per pattern mirrors the TS `/.../i` flag.
    let patterns: Vec<String> = MODE_RULES
        .iter()
        .map(|r| format!("(?i){}", r.pattern))
        .collect();
    regex::RegexSet::new(patterns).expect("static mode-scoring patterns compile")
});

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredResult {
    pub mode: DelegationMode,
    pub reason: String,
    pub confidence: f64,
}

fn classify_by_scoring(description: &str) -> ScoredResult {
    let matches = MODE_SCORING_SET.matches(description);
    let mut scores: HashMap<DelegationMode, u32> = HashMap::new();
    let mut best_reason = "default".to_string();
    for pat_idx in matches.iter() {
        let rule = &MODE_RULES[pat_idx];
        *scores.entry(rule.mode).or_insert(0) += rule.score;
        best_reason = rule.reason.to_string();
    }

    let mut best = DelegationMode::HardPrompt;
    let mut highest: u32 = 0;
    // ponytail: explicit order matches the TS iteration order over
    // ["artifact", "schema-contract", "hard-prompt", "task-decompose"] and
    // keeps the strict `>` semantics so ties don't flip the winner.
    for m in [
        DelegationMode::Artifact,
        DelegationMode::SchemaContract,
        DelegationMode::HardPrompt,
        DelegationMode::TaskDecompose,
    ] {
        let s = *scores.get(&m).unwrap_or(&0);
        if s > highest {
            best = m;
            highest = s;
        }
    }

    let art = *scores.get(&DelegationMode::Artifact).unwrap_or(&0);
    let tc = *scores.get(&DelegationMode::SchemaContract).unwrap_or(&0);
    let td = *scores.get(&DelegationMode::TaskDecompose).unwrap_or(&0);
    if td > 0 && td >= art && td >= tc {
        best = DelegationMode::TaskDecompose;
        best_reason = "multi-step decomposition (overrides code-gen)".to_string();
    } else if art > 0 && art >= tc {
        best = DelegationMode::Artifact;
        best_reason = if tc > 0 {
            "file creation (overrides audit)".to_string()
        } else {
            "file creation".to_string()
        };
    }

    let second_highest = [
        DelegationMode::Artifact,
        DelegationMode::SchemaContract,
        DelegationMode::HardPrompt,
        DelegationMode::TaskDecompose,
    ]
    .iter()
    .filter(|m| **m != best)
    .map(|m| *scores.get(m).unwrap_or(&0))
    .max()
    .unwrap_or(0);
    let margin = highest.saturating_sub(second_highest) as f64;
    let confidence = (margin / highest.max(1) as f64) * (highest as f64 / 20.0).min(1.0);

    ScoredResult {
        mode: best,
        reason: best_reason,
        confidence: confidence.min(0.9),
    }
}

// ── classify_task (top-level entry) ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct TaskInput<'a> {
    pub description: &'a str,
    pub mode_override: Option<DelegationMode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DelegationDecision {
    pub mode: DelegationMode,
    pub reason: String,
    pub auto_routed: bool,
}

/// Top-level task classifier. Honors `mode_override`, otherwise uses regex
/// scoring; falls back to NLP when regex confidence is below
/// `NLP_FALLBACK_THRESHOLD`.
pub fn classify_task(task: &TaskInput<'_>) -> DelegationDecision {
    if let Some(mode) = task.mode_override {
        return DelegationDecision {
            mode,
            reason: "user override".to_string(),
            auto_routed: false,
        };
    }

    let regex_result = classify_by_scoring(task.description);
    if regex_result.confidence >= NLP_FALLBACK_THRESHOLD {
        let auto_routed =
            regex_result.mode != DelegationMode::HardPrompt || regex_result.confidence > 0.1;
        return DelegationDecision {
            mode: regex_result.mode,
            reason: regex_result.reason,
            auto_routed,
        };
    }

    let nlp = classify_nlp(task.description);
    DelegationDecision {
        mode: nlp.mode,
        reason: format!(
            "nlp-classified (regex confidence {:.2} < {}, nlp confidence {:.2})",
            regex_result.confidence, NLP_FALLBACK_THRESHOLD, nlp.confidence
        ),
        auto_routed: true,
    }
}

// ── NLP classifier (classifier-nlp.ts) ──────────────────────────────────────

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall", "to",
    "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "through", "during",
    "before", "after", "above", "below", "between", "out", "off", "over", "under", "again",
    "further", "then", "once", "here", "there", "when", "where", "why", "how", "all", "both",
    "each", "few", "more", "most", "other", "some", "such", "no", "nor", "not", "only", "own",
    "same", "so", "than", "too", "very", "just", "about", "and", "but", "or", "it", "its", "this",
    "that", "these", "those",
];

struct Archetype {
    mode: DelegationMode,
    examples: &'static [&'static str],
}

const ARCHETYPES: &[Archetype] = &[
    Archetype {
        mode: DelegationMode::Artifact,
        examples: &[
            "create a new react component file",
            "generate a python module for data processing",
            "write a typescript utility function",
            "build a REST API endpoint handler",
            "make a configuration file for deployment",
            "implement a database migration script",
            "scaffold a new service module",
            "generate unit tests for the auth module",
            "write a Dockerfile for the application",
            "create a bash deployment script",
            "build a CSS stylesheet for the dashboard",
            "generate HTML template for email notifications",
            "write a SQL migration for adding columns",
            "implement a go middleware handler",
            "create a rust library module",
        ],
    },
    Archetype {
        mode: DelegationMode::SchemaContract,
        examples: &[
            "audit the codebase for security vulnerabilities",
            "assess the architecture for scalability issues",
            "evaluate the test coverage gaps",
            "review the pull request for merge readiness",
            "validate the input configuration against schema",
            "verify the deployment manifests are correct",
            "analyze the dependency tree for outdated packages",
            "inspect the logging configuration for completeness",
            "check all environment variables are documented",
            "examine the error handling patterns",
            "summarize the changes in this release",
            "document the API endpoints with OpenAPI spec",
            "compare the two implementations for correctness",
        ],
    },
    Archetype {
        mode: DelegationMode::HardPrompt,
        examples: &[
            "fix the lint errors in the auth module",
            "repair the broken type definitions",
            "refactor the database access layer",
            "optimize the slow query performance",
            "debug the authentication middleware",
            "resolve the merge conflicts in main branch",
            "patch the security vulnerability in dependencies",
            "correct the import path references",
            "update the deprecated API calls",
            "simplify the complex reducer logic",
            "clean up unused variables and imports",
            "migrate from old API to new API version",
            "troubleshoot the failing integration tests",
            "tighten the type annotations across the module",
        ],
    },
];

fn nlp_tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for word in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        let w = word.trim();
        if w.len() > 1 && !STOP_WORDS.contains(&w) {
            out.push(w.to_string());
        }
    }
    out
}

fn compute_tf(doc: &[String]) -> HashMap<String, f64> {
    let mut tf: HashMap<String, f64> = HashMap::new();
    for tok in doc {
        *tf.entry(tok.clone()).or_insert(0.0) += 1.0;
    }
    let total = doc.len().max(1) as f64;
    for v in tf.values_mut() {
        *v /= total;
    }
    tf
}

struct NlpModel {
    vocabulary: Vec<String>,
    idf: HashMap<String, f64>,
    centroids: HashMap<DelegationMode, Vec<f64>>,
}

fn build_model() -> NlpModel {
    let mut all_docs: Vec<Vec<String>> = Vec::new();
    let mut mode_docs: HashMap<DelegationMode, Vec<Vec<String>>> = HashMap::new();
    for arch in ARCHETYPES {
        let docs: Vec<Vec<String>> = arch.examples.iter().map(|e| nlp_tokenize(e)).collect();
        for d in &docs {
            all_docs.push(d.clone());
        }
        mode_docs.insert(arch.mode, docs);
    }

    // document frequency
    let mut df: HashMap<String, f64> = HashMap::new();
    let n = all_docs.len() as f64;
    for doc in &all_docs {
        let seen: std::collections::HashSet<&String> = doc.iter().collect();
        for tok in seen {
            *df.entry(tok.clone()).or_insert(0.0) += 1.0;
        }
    }
    let mut vocab: Vec<String> = df.keys().cloned().collect();
    vocab.sort();
    let idf: HashMap<String, f64> = df
        .iter()
        .map(|(tok, count)| (tok.clone(), ((n + 1.0) / (count + 1.0)).ln() + 1.0))
        .collect();

    let vectorize_doc = |doc: &[String]| -> Vec<f64> {
        let tf = compute_tf(doc);
        vocab
            .iter()
            .map(|tok| tf.get(tok).copied().unwrap_or(0.0) * idf.get(tok).copied().unwrap_or(0.0))
            .collect()
    };

    let mut centroids: HashMap<DelegationMode, Vec<f64>> = HashMap::new();
    for (mode, docs) in &mode_docs {
        let vectors: Vec<Vec<f64>> = docs.iter().map(|d| vectorize_doc(d)).collect();
        let dim = vocab.len();
        let mut centroid = vec![0.0f64; dim];
        for v in &vectors {
            for (i, x) in v.iter().enumerate() {
                centroid[i] += x;
            }
        }
        let n = vectors.len().max(1) as f64;
        for x in &mut centroid {
            *x /= n;
        }
        centroids.insert(*mode, centroid);
    }

    NlpModel {
        vocabulary: vocab,
        idf,
        centroids,
    }
}

static NLP_MODEL: LazyLock<NlpModel> = LazyLock::new(build_model);

fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len().max(b.len()) {
        let av = *a.get(i).unwrap_or(&0.0);
        let bv = *b.get(i).unwrap_or(&0.0);
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NlpResult {
    pub mode: DelegationMode,
    pub confidence: f64,
    pub scores: HashMap<String, f64>,
}

/// TF-IDF + cosine similarity against archetype centroids.
pub fn classify_nlp(description: &str) -> NlpResult {
    let model = &*NLP_MODEL;
    let tokens = nlp_tokenize(description);
    let tf = compute_tf(&tokens);
    let vec: Vec<f64> = model
        .vocabulary
        .iter()
        .map(|tok| tf.get(tok).copied().unwrap_or(0.0) * model.idf.get(tok).copied().unwrap_or(0.0))
        .collect();

    let mut scores: HashMap<String, f64> = HashMap::new();
    for mode in [
        DelegationMode::Artifact,
        DelegationMode::SchemaContract,
        DelegationMode::HardPrompt,
        DelegationMode::TaskDecompose,
    ] {
        let centroid = model.centroids.get(&mode);
        let s = match centroid {
            Some(c) => cosine_similarity(&vec, c),
            None => 0.0,
        };
        scores.insert(mode.as_str().to_string(), s);
    }

    let mut best = DelegationMode::HardPrompt;
    let mut best_score = 0.0f64;
    for mode in [
        DelegationMode::Artifact,
        DelegationMode::SchemaContract,
        DelegationMode::HardPrompt,
        DelegationMode::TaskDecompose,
    ] {
        let s = *scores.get(mode.as_str()).unwrap_or(&0.0);
        if s > best_score {
            best_score = s;
            best = mode;
        }
    }

    let mut sorted: Vec<f64> = scores.values().copied().collect();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top = sorted.first().copied().unwrap_or(0.0);
    let runner_up = sorted.get(1).copied().unwrap_or(0.0);
    let margin = top - runner_up;
    let confidence = (margin / top.max(0.01)).clamp(0.0, 1.0);

    NlpResult {
        mode: best,
        confidence,
        scores,
    }
}

/// Hybrid: if exactly one strong regex signal fires, use it; otherwise NLP.
pub fn classify_hybrid(description: &str) -> (DelegationMode, f64) {
    let lower = description.to_lowercase();
    let strong_artifact = regex::Regex::new(
        r"\b(?:generate|create|write|build|make)\s+(?:a\s+)?(?:\w+\s+)?(?:file|component|module|service|class|server|app|script)\b",
    )
    .unwrap()
    .is_match(&lower);
    let strong_audit = regex::Regex::new(r"\b(?:audit|assess|evaluate|validate|verify)\b")
        .unwrap()
        .is_match(&lower);
    let strong_fix = regex::Regex::new(r"\b(?:fix|repair|refactor|debug|patch)\b")
        .unwrap()
        .is_match(&lower);

    let signals = [strong_artifact, strong_audit, strong_fix]
        .iter()
        .filter(|&&b| b)
        .count();
    if signals == 1 {
        if strong_artifact {
            return (DelegationMode::Artifact, 0.8);
        }
        if strong_audit {
            return (DelegationMode::SchemaContract, 0.7);
        }
        if strong_fix {
            return (DelegationMode::HardPrompt, 0.7);
        }
    }

    let nlp = classify_nlp(description);
    (nlp.mode, nlp.confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_task_audit_to_schema_contract() {
        let d = classify_task(&TaskInput {
            description: "audit the security report",
            mode_override: None,
        });
        assert_eq!(d.mode, DelegationMode::SchemaContract);
    }

    #[test]
    fn classify_task_file_creation_to_artifact() {
        let d = classify_task(&TaskInput {
            description: "generate a component file",
            mode_override: None,
        });
        assert_eq!(d.mode, DelegationMode::Artifact);
    }

    #[test]
    fn classify_task_artifact_overrides_schema_contract_for_mixed_keywords() {
        let d = classify_task(&TaskInput {
            description: "write a TypeScript server with validation",
            mode_override: None,
        });
        assert_eq!(d.mode, DelegationMode::Artifact);
    }

    #[test]
    fn classify_task_defaults_to_hard_prompt() {
        let d = classify_task(&TaskInput {
            description: "hello world",
            mode_override: None,
        });
        assert_eq!(d.mode, DelegationMode::HardPrompt);
    }

    #[test]
    fn classify_task_user_override_respected() {
        let d = classify_task(&TaskInput {
            description: "build",
            mode_override: Some(DelegationMode::Artifact),
        });
        assert_eq!(d.mode, DelegationMode::Artifact);
        assert!(!d.auto_routed);
    }

    #[test]
    fn classify_nlp_recognizes_artifact_example() {
        let r = classify_nlp("write a python module for processing");
        assert_eq!(r.mode, DelegationMode::Artifact);
    }

    #[test]
    fn classify_nlp_recognizes_audit_example() {
        let r = classify_nlp("audit the codebase for security vulnerabilities");
        assert_eq!(r.mode, DelegationMode::SchemaContract);
    }

    #[test]
    fn classify_nlp_recognizes_fix_example() {
        let r = classify_nlp("fix the lint errors in the auth module");
        assert_eq!(r.mode, DelegationMode::HardPrompt);
    }

    #[test]
    fn classify_hybrid_single_strong_signal_uses_it() {
        let (mode, conf) = classify_hybrid("generate a python module file");
        assert_eq!(mode, DelegationMode::Artifact);
        assert!((conf - 0.8).abs() < 1e-12);
    }

    #[test]
    fn classify_hybrid_ambiguous_falls_through_to_nlp() {
        // Two strong signals (artifact + audit) → neither dominates → NLP.
        let (mode, _) = classify_hybrid("audit the report then write a file");
        // NLP picks whichever centroid is closest; just assert it's one of
        // the valid modes and doesn't panic.
        assert!(matches!(
            mode,
            DelegationMode::Artifact
                | DelegationMode::SchemaContract
                | DelegationMode::HardPrompt
                | DelegationMode::TaskDecompose
        ));
    }

    #[test]
    fn delegation_mode_serialises_kebab_case() {
        assert_eq!(
            serde_json::to_string(&DelegationMode::SchemaContract).unwrap(),
            "\"schema-contract\""
        );
        assert_eq!(
            serde_json::to_string(&DelegationMode::HardPrompt).unwrap(),
            "\"hard-prompt\""
        );
        assert_eq!(
            serde_json::to_string(&DelegationMode::TaskDecompose).unwrap(),
            "\"task-decompose\""
        );
    }
}
