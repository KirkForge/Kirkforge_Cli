//! R1 — port of `memory-palace/src/routing-engine.ts`.
//!
//! FNV-1a hashed bag-of-words vectorizer + cosine similarity + a coarse
//! regex task-family classifier. Used by the memory store to fingerprint
//! tasks and find similar prior observations. Pure: no fs, no model calls.

use serde::{Deserialize, Serialize};

use crate::classifier::DelegationMode;

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "using", "task", "file", "files",
    "write", "create", "build", "make",
];

// ponytail: the TS regex `[a-z0-9][a-z0-9._-]{2,}` is ASCII-only, so a byte
// scan is equivalent to the JS charCodeAt loop and avoids UTF-16 surrogate
// edge cases. First char must be alnum; 2+ trailing chars may be alnum / dot
// / dash / underscore (min total length 3).
const TOKEN_RE: &str = r"[a-z0-9][a-z0-9._-]{2,}";

/// Regex-based coarse task-family classifier.
pub fn detect_family(description: &str) -> &'static str {
    let lower = description.to_lowercase();
    if contains_any(&lower, &["web", "http", "server", "endpoint", "api"]) {
        "web"
    } else if contains_any(&lower, &["script", "cli", "command", "shell"]) {
        "script"
    } else if contains_any(&lower, &["test", "spec", "verify", "check"]) {
        "testing"
    } else if contains_any(&lower, &["data", "parse", "scrape", "csv", "json"]) {
        "data"
    } else if contains_any(&lower, &["fix", "debug", "repair", "patch"]) {
        "debugging"
    } else {
        "general"
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Stopword-filtered tokenizer. Capped at 40 unique tokens. Order-preserving
/// dedup matches JS `Set` iteration order for first-seen wins.
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let re = regex::Regex::new(TOKEN_RE).expect("static token regex compiles");
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in re.find_iter(&lower) {
        let s = m.as_str();
        if seen.insert(s) && !STOP_WORDS.contains(&s) {
            out.push(s.to_string());
            if out.len() == 40 {
                break;
            }
        }
    }
    out
}

/// FNV-1a-hashed bag-of-words vector. Default 64 dimensions.
///
/// `Math.imul(h, 16777619)` in JS returns a signed 32-bit result; `Math.abs`
/// of that is then taken before `% dim`. We mirror the bit layout via
/// `wrapping_mul` and `wrapping_abs` so hashes match TS byte-for-byte on the
/// ASCII tokens the tokenizer can produce.
pub fn vectorize(tokens: &[String], dimensions: usize) -> Vec<u32> {
    let mut v = vec![0u32; dimensions];
    for tok in tokens {
        let mut hash: u32 = 2166136261;
        for b in tok.bytes() {
            hash ^= u32::from(b);
            hash = hash.wrapping_mul(16777619);
        }
        let idx = (hash as i32).wrapping_abs() as usize % dimensions;
        v[idx] += 1;
    }
    v
}

/// Cosine similarity. Zero-vectors return 0 (matches TS guard `an===0||bn===0`).
pub fn cosine(a: &[u32], b: &[u32]) -> f64 {
    let len = a.len().max(b.len());
    let mut dot = 0.0f64;
    let mut an = 0.0f64;
    let mut bn = 0.0f64;
    for i in 0..len {
        let av = *a.get(i).unwrap_or(&0) as f64;
        let bv = *b.get(i).unwrap_or(&0) as f64;
        dot += av * bv;
        an += av * av;
        bn += bv * bv;
    }
    if an == 0.0 || bn == 0.0 {
        0.0
    } else {
        dot / (an.sqrt() * bn.sqrt())
    }
}

/// Wraps `tokenize` + `vectorize` + `detect_family`.
pub fn fingerprint_task(description: &str) -> Fingerprint {
    let tokens = tokenize(description);
    let vector = vectorize(&tokens, 64);
    let task_family = detect_family(description);
    Fingerprint {
        tokens,
        vector,
        task_family: task_family.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub tokens: Vec<String>,
    pub vector: Vec<u32>,
    pub task_family: String,
}

/// Coerce arbitrary input into one of the three valid outcomes. Defaults to
/// "error" (matches TS).
pub fn normalize_outcome(value: Option<&str>) -> &'static str {
    match value {
        Some("pass") => "pass",
        Some("fail") => "fail",
        Some("error") => "error",
        _ => "error",
    }
}

// ── buildEmpiricalRecommendation ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub description: String,
    #[serde(default)]
    pub vector: Option<Vec<u32>>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub task_family: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub source_of_truth: Option<String>,
    #[serde(default)]
    pub routing_lesson: Option<String>,
    #[serde(default)]
    pub outcome_class: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub tokens: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCase {
    pub task_family: String,
    pub language: String,
    pub mode: String,
    pub model: String,
    pub outcome: String,
    pub outcome_class: String,
    pub source_of_truth: String,
    pub reason: String,
    pub tokens: f64,
    pub duration_ms: f64,
    pub similarity: f64,
    pub truth_weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingBias {
    pub prefer: Vec<String>,
    pub avoid: Vec<String>,
    pub confidence: f64,
    pub influence: f64,
    pub evidence: usize,
    pub similar_cases: Vec<RoutingCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub mode: String,
    pub model: String,
    pub confidence: f64,
    pub evidence: usize,
    pub expected_tokens: i64,
    pub score: f64,
    pub routing_bias: RoutingBias,
}

#[derive(Default, Clone)]
struct ModeStats {
    pass_: f64,
    fail_: f64,
    tokens: f64,
    score: f64,
}

#[derive(Default, Clone)]
struct ModeCount {
    pass_: f64,
    fail_: f64,
    score: f64,
}

/// Build a routing recommendation from prior observations. Aggregates
/// pass/fail-weighted scores per model and per mode, applying a "truth weight"
/// (task-validator=2.0, verifier=1.0). Returns `None` when no observation is
/// similar enough (similarity >= 0.25).
pub fn build_empirical_recommendation(
    task_description: &str,
    observations: &[Observation],
    worker_model: Option<&str>,
) -> Option<Recommendation> {
    let query = fingerprint_task(task_description);

    let mut similar: Vec<(usize, f64)> = observations
        .iter()
        .enumerate()
        .filter_map(|(i, obs)| {
            let vector = obs.vector.clone().unwrap_or_else(|| {
                let mut seed: Vec<String> = Vec::new();
                if let Some(lang) = &obs.language {
                    seed.push(lang.clone());
                }
                if let Some(fam) = &obs.task_family {
                    seed.push(fam.clone());
                }
                seed.extend(tokenize(&obs.description));
                vectorize(&seed, 64)
            });
            let similarity = cosine(&query.vector, &vector);
            let same_family = obs.task_family.as_deref() == Some(query.task_family.as_str());
            let bonus = if same_family { 0.25 } else { 0.0 };
            let score = (similarity + bonus).min(1.0);
            (score >= 0.25).then_some((i, score))
        })
        .collect();

    similar.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    similar.truncate(12);
    if similar.is_empty() {
        return None;
    }

    let mut by_model: std::collections::HashMap<String, ModeStats> =
        std::collections::HashMap::new();
    let mut by_mode: std::collections::HashMap<String, ModeCount> =
        std::collections::HashMap::new();
    let mut cases: Vec<RoutingCase> = Vec::new();

    for (idx, sim) in &similar {
        let obs = &observations[*idx];
        let model = obs.model.clone().unwrap_or_else(|| "unknown".to_string());
        let mode = obs
            .mode
            .clone()
            .unwrap_or_else(|| "hard-prompt".to_string());
        let outcome = normalize_outcome(obs.outcome.as_deref()).to_string();
        let source_of_truth = obs
            .source_of_truth
            .clone()
            .unwrap_or_else(|| "verifier".to_string());
        let routing_lesson_raw = obs.routing_lesson.clone().unwrap_or_default();
        let routing_lesson = if !routing_lesson_raw.is_empty() {
            routing_lesson_raw
        } else {
            match outcome.as_str() {
                "pass" => "reward".to_string(),
                "fail" => "punish".to_string(),
                _ => "neutral".to_string(),
            }
        };
        let truth_factor = if source_of_truth == "task-validator" {
            2.0
        } else {
            1.0
        };
        let weight = sim * truth_factor;
        let model_stats = by_model.entry(model.clone()).or_default();
        let mode_stats = by_mode.entry(mode.clone()).or_default();
        match routing_lesson.as_str() {
            "reward" => {
                model_stats.pass_ += weight;
                mode_stats.pass_ += weight;
            }
            "punish" => {
                model_stats.fail_ += weight;
                mode_stats.fail_ += weight;
            }
            "neutral" => {}
            _ => match outcome.as_str() {
                "pass" => {
                    model_stats.pass_ += weight;
                    mode_stats.pass_ += weight;
                }
                "fail" => {
                    model_stats.fail_ += weight;
                    mode_stats.fail_ += weight;
                }
                _ => {}
            },
        }
        model_stats.tokens += obs.tokens.unwrap_or(0.0) * weight;
        model_stats.score += weight;
        mode_stats.score += weight;
        cases.push(RoutingCase {
            task_family: obs
                .task_family
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            language: obs
                .language
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            mode: mode.clone(),
            model: model.clone(),
            outcome: outcome.clone(),
            outcome_class: obs
                .outcome_class
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            source_of_truth: source_of_truth.clone(),
            reason: obs.reason.clone().unwrap_or_else(|| outcome.clone()),
            tokens: obs.tokens.unwrap_or(0.0),
            duration_ms: obs.duration_ms.unwrap_or(0.0),
            similarity: (*sim * 1000.0).round() / 1000.0,
            truth_weight: truth_factor,
        });
    }

    let mut ranked_models: Vec<(String, f64, f64, i64)> = by_model
        .iter()
        .map(|(model, d)| {
            let total = d.pass_ + d.fail_;
            let pass_rate = d.pass_ / total.max(0.001);
            let evidence = total;
            let expected_tokens = (d.tokens / d.score.max(0.001)).round() as i64;
            (model.clone(), pass_rate, evidence, expected_tokens)
        })
        .collect();
    ranked_models.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut ranked_modes: Vec<(String, f64, f64)> = by_mode
        .iter()
        .map(|(mode, d)| {
            let total = d.pass_ + d.fail_;
            let pass_rate = d.pass_ / total.max(0.001);
            (mode.clone(), pass_rate, total)
        })
        .collect();
    ranked_modes.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });

    let prefer: Vec<String> = ranked_models
        .iter()
        .filter(|(_, pr, _, _)| *pr >= 0.62)
        .take(2)
        .map(|(m, _, _, _)| m.clone())
        .collect();
    let avoid: Vec<String> = ranked_models
        .iter()
        .filter(|(_, pr, ev, _)| *pr <= 0.38 && *ev >= 0.35)
        .take(3)
        .map(|(m, _, _, _)| m.clone())
        .collect();

    let best_model = prefer
        .first()
        .cloned()
        .or_else(|| ranked_models.first().map(|(m, _, _, _)| m.clone()))
        .or_else(|| worker_model.map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let best_mode = ranked_modes
        .first()
        .map(|(m, _, _)| m.clone())
        .unwrap_or_else(|| "hard-prompt".to_string());
    let best_model_evidence = ranked_models
        .iter()
        .find(|(m, _, _, _)| *m == best_model)
        .map(|(_, _, ev, _)| *ev)
        .unwrap_or(0.0);
    let best_model_tokens = ranked_models
        .iter()
        .find(|(m, _, _, _)| *m == best_model)
        .map(|(_, _, _, t)| *t)
        .unwrap_or(0);
    let evidence = similar.len();
    let confidence = (best_model_evidence / (best_model_evidence + 2.0)).min(0.9);

    // mode-driven mode override: TS recommends the best mode's mode for the
    // routed model decision; the recommendation's `mode` field tracks the
    // empirical best mode, while the model defaults to the worker's.
    let _ = DelegationMode::Artifact; // keep enum import live for symmetry with TS
    Some(Recommendation {
        mode: best_mode,
        model: worker_model.map(|s| s.to_string()).unwrap_or(best_model),
        confidence,
        evidence,
        expected_tokens: best_model_tokens,
        score: ranked_modes.first().map(|(_, pr, _)| *pr).unwrap_or(0.0),
        routing_bias: RoutingBias {
            prefer,
            avoid,
            confidence,
            influence: 0.25,
            evidence,
            similar_cases: cases.into_iter().take(5).collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_dedups_and_filters_stopwords() {
        let toks = tokenize("The task build task file file_file file-file");
        // "task","file","build" are stop-words; "file_file" / "file-file"
        // survive (length >= 3, allowed chars). "The" is filtered by regex
        // (length 3 but uppercase → lowercased to "the" → stopword).
        assert!(!toks.contains(&"task".to_string()));
        assert!(!toks.contains(&"file".to_string()));
        assert!(toks.contains(&"file_file".to_string()));
        assert!(toks.contains(&"file-file".to_string()));
    }

    #[test]
    fn tokenize_caps_at_40_unique_tokens() {
        let mut words = Vec::new();
        for i in 0..200 {
            words.push(format!("tok{i:03}"));
        }
        let text = words.join(" ");
        let toks = tokenize(&text);
        assert_eq!(toks.len(), 40);
    }

    #[test]
    fn tokenize_requires_min_length_3() {
        let toks = tokenize("a ab abc abcd");
        // first char must be alnum, total length >= 3
        assert_eq!(toks, vec!["abc".to_string(), "abcd".to_string()]);
    }

    #[test]
    fn vectorize_is_zero_for_empty_input() {
        let v = vectorize(&[], 64);
        assert_eq!(v, vec![0u32; 64]);
    }

    #[test]
    fn vectorize_buckets_repeat_tokens() {
        // FNV-1a is deterministic; same token always lands in the same bucket.
        let v = vectorize(&["hello".to_string()], 64);
        let sum: u32 = v.iter().sum();
        assert_eq!(sum, 1);
        let toks: Vec<String> = std::iter::repeat_n("hello".to_string(), 5).collect();
        let v2 = vectorize(&toks, 64);
        let sum2: u32 = v2.iter().sum();
        assert_eq!(sum2, 5);
    }

    #[test]
    fn cosine_zero_vectors_return_zero() {
        assert_eq!(cosine(&[0, 0, 0], &[1, 2, 3]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vectorize(&["hello".to_string(), "world".to_string()], 16);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn cosine_unequal_lengths_pads_with_zero() {
        assert!((cosine(&[1, 0, 0], &[1]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn detect_family_matches_keywords() {
        assert_eq!(detect_family("build a web server endpoint"), "web");
        assert_eq!(detect_family("run a shell command"), "script");
        assert_eq!(detect_family("write a test spec"), "testing");
        assert_eq!(detect_family("parse the csv data"), "data");
        assert_eq!(detect_family("fix the bug"), "debugging");
        assert_eq!(detect_family("hello world"), "general");
    }

    #[test]
    fn fingerprint_task_combines_helpers() {
        let fp = fingerprint_task("fix the broken web server");
        assert!(!fp.tokens.is_empty());
        assert_eq!(fp.vector.len(), 64);
        assert_eq!(fp.task_family, "web");
    }

    #[test]
    fn normalize_outcome_coerces_unknown() {
        assert_eq!(normalize_outcome(Some("pass")), "pass");
        assert_eq!(normalize_outcome(Some("fail")), "fail");
        assert_eq!(normalize_outcome(Some("error")), "error");
        assert_eq!(normalize_outcome(Some("bogus")), "error");
        assert_eq!(normalize_outcome(None), "error");
    }

    #[test]
    fn build_recommendation_returns_none_when_no_evidence() {
        assert!(build_empirical_recommendation("hello world", &[], None).is_none());
    }

    #[test]
    fn build_recommendation_ranks_preferred_model() {
        let obs = vec![
            Observation {
                description: "write a python module".into(),
                vector: None,
                language: Some("python".into()),
                task_family: Some("general".into()),
                mode: Some("artifact".into()),
                model: Some("good-model".into()),
                outcome: Some("pass".into()),
                source_of_truth: Some("verifier".into()),
                routing_lesson: None,
                outcome_class: None,
                reason: None,
                tokens: Some(100.0),
                duration_ms: Some(0.0),
            },
            Observation {
                description: "write a python module".into(),
                vector: None,
                language: Some("python".into()),
                task_family: Some("general".into()),
                mode: Some("artifact".into()),
                model: Some("bad-model".into()),
                outcome: Some("fail".into()),
                source_of_truth: Some("verifier".into()),
                routing_lesson: None,
                outcome_class: None,
                reason: None,
                tokens: Some(100.0),
                duration_ms: Some(0.0),
            },
        ];
        let rec = build_empirical_recommendation("write a python module", &obs, None)
            .expect("similar observations exist");
        assert_eq!(rec.model, "good-model");
        assert!(rec.routing_bias.prefer.contains(&"good-model".to_string()));
        assert!(rec.routing_bias.avoid.contains(&"bad-model".to_string()));
        assert!(rec.evidence >= 1);
    }
}
