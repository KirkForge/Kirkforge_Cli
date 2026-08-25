//! `MemoryStore` facade. Port of `memory-palace/src/store.ts`.
//!
//! Wraps a `MemoryAdapter` and exposes the orchestrator-friendly
//! write/recall surface. Uses `kf-routing` for `tokenize`/`vectorize`/
//! `detect_family`/`build_empirical_recommendation` (ported in WO 29.3).

use std::sync::Mutex;

use anyhow::Result;
use kf_routing::{
    build_empirical_recommendation, detect_family, tokenize, vectorize, Observation, Recommendation,
};
use serde_json::{json, Value};

use crate::adapters::{file::FileAdapter, sqlite::SqliteAdapter, MemoryAdapter};
use crate::time::{cheap_random_u32, iso_now, iso_now_minus_ms, now_millis};
use crate::types::{
    EmissionRow, EmittedFileRecord, MemoryObject, MemoryQuery, RunRecord, RunRow,
    TaskObservationInput,
};

/// Options for [`MemoryStore::new`] / [`MemoryStore::create`].
#[derive(Debug, Clone, Default)]
pub struct MemoryStoreOptions {
    /// TTL in milliseconds for task observations. 0 = disabled.
    pub ttl_ms: i64,
    /// Maximum number of entries before eviction triggers. 0 = disabled.
    pub max_entries: usize,
}

pub struct MemoryStore {
    adapter: Box<dyn MemoryAdapter>,
    options: MemoryStoreOptions,
    /// Serializes write/recall entry-points. The TS code is async but
    /// single-threaded via the JS event loop; the Rust port uses a Mutex to
    /// preserve "one logical operation at a time" semantics.
    lock: Mutex<()>,
}

impl MemoryStore {
    pub fn new<A: MemoryAdapter + 'static>(adapter: A, options: MemoryStoreOptions) -> Self {
        Self {
            adapter: Box::new(adapter),
            options,
            lock: Mutex::new(()),
        }
    }

    /// Construct from a path. Tries SQLite first; falls back to FileAdapter
    /// if SQLite fails to open (matches TS `MemoryStore.create`).
    pub fn create<P: Into<std::path::PathBuf>>(
        db_path: P,
        options: MemoryStoreOptions,
    ) -> Result<Self> {
        let path = db_path.into();
        match SqliteAdapter::open(&path) {
            Ok(adapter) => Ok(Self::new(adapter, options)),
            Err(_) => {
                let mut fallback = path.into_os_string();
                fallback.push(".json");
                let adapter = FileAdapter::new(std::path::PathBuf::from(fallback));
                Ok(Self::new(adapter, options))
            }
        }
    }

    pub fn adapter(&self) -> &dyn MemoryAdapter {
        self.adapter.as_ref()
    }
    pub fn ttl_ms(&self) -> i64 {
        self.options.ttl_ms
    }
    pub fn max_entries(&self) -> usize {
        self.options.max_entries
    }

    /// Evict entries older than TTL. Returns count evicted.
    pub fn evict_expired(&self) -> Result<usize> {
        if self.options.ttl_ms <= 0 {
            return Ok(0);
        }
        let _g = self.lock.lock().expect("store lock poisoned");
        let cutoff = iso_now_minus_ms(self.options.ttl_ms);
        let q = MemoryQuery {
            limit: Some(100_000),
            ..Default::default()
        };
        let all = self.adapter.query(&q)?;
        let to_evict: Vec<&MemoryObject> = all
            .iter()
            .filter(|o| o.timestamp.as_str() < cutoff.as_str())
            .collect();
        for o in &to_evict {
            self.adapter.delete(&o.id)?;
        }
        Ok(to_evict.len())
    }

    /// Evict oldest entries when over max_entries. Returns count evicted.
    pub fn evict_overflow(&self) -> Result<usize> {
        if self.options.max_entries == 0 {
            return Ok(0);
        }
        let _g = self.lock.lock().expect("store lock poisoned");
        let stats = self.adapter.stats()?;
        let excess = stats.total_objects.saturating_sub(self.options.max_entries);
        if excess == 0 {
            return Ok(0);
        }
        // query returns newest-first (DESC by timestamp); drop the newest
        // max_entries and evict the oldest tail.
        let q = MemoryQuery {
            limit: Some(stats.total_objects),
            ..Default::default()
        };
        let all = self.adapter.query(&q)?;
        let to_evict = &all[self.options.max_entries.min(all.len())..];
        for o in to_evict {
            self.adapter.delete(&o.id)?;
        }
        Ok(to_evict.len())
    }

    pub fn write_task_observation(&self, params: &TaskObservationInput) -> Result<()> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let tokens = tokenize(&params.description);
        let vector = vectorize(&tokens, 64);
        let inferred_outcome = params
            .outcome
            .clone()
            .unwrap_or_else(|| match params.task_pass {
                Some(true) => "pass".to_string(),
                Some(false) => "fail".to_string(),
                None => "error".to_string(),
            });
        let id = format!(
            "observation-{}-{}-{:x}",
            params.task_id,
            now_millis(),
            cheap_random_u32()
        );
        let task_family = params
            .task_family
            .clone()
            .unwrap_or_else(|| detect_family(&params.description).to_string());
        let reason = params
            .reason
            .clone()
            .unwrap_or_else(|| match inferred_outcome.as_str() {
                "pass" => "task passed".to_string(),
                "fail" => "task tests failed".to_string(),
                _ => "task outcome unknown".to_string(),
            });
        let mut properties = json!({
            "language": params.language,
            "taskFamily": task_family,
            "mode": params.mode,
            "model": params.model,
            "providerKey": params.provider_key,
            "providerType": params.provider_type,
            // baseUrl intentionally excluded — may contain credentials.
            "promptShape": params.prompt_shape,
            "verifierOverall": params.verifier_overall,
            "finalAction": params.final_action,
            "taskPass": params.task_pass,
            "outcome": inferred_outcome,
            "reason": reason,
            "tokens": params.tokens,
            "durationMs": params.duration_ms,
            "turns": params.turns,
            "finalVerdict": params.final_verdict,
            "sourceOfTruth": params.source_of_truth,
            "taskValidation": params.task_validation,
            "tokens_description": tokens,
            "vector": vector,
        });
        if let Some(val) = &params.outcome_class {
            properties["outcomeClass"] = json!(val);
        }
        if let Some(val) = &params.routing_lesson {
            properties["routingLesson"] = json!(val);
        }

        let mut tags = vec![
            params.language.clone(),
            params.mode.clone(),
            inferred_outcome,
        ];
        tags.retain(|s| !s.is_empty());

        let obj = MemoryObject {
            id,
            kind: "task-observation".to_string(),
            task_id: params.task_id.clone(),
            run_id: None,
            timestamp: iso_now(),
            description: params.description.clone(),
            properties,
            tags,
        };
        self.adapter.write(&obj)
    }

    pub fn write_decomposition(
        &self,
        task_id: &str,
        description: &str,
        tasks: &[Value],
        language: &str,
    ) -> Result<()> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let id = format!("decomp-{task_id}-{}", now_millis());
        let obj = MemoryObject {
            id,
            kind: "task-decomposition".to_string(),
            task_id: task_id.to_string(),
            run_id: None,
            timestamp: iso_now(),
            description: description.to_string(),
            properties: json!({
                "language": language,
                "taskCount": tasks.len(),
                "tasks": tasks,
            }),
            tags: vec!["decomposition".to_string(), language.to_string()],
        };
        self.adapter.write(&obj)
    }

    /// Returns `(task_id, description, tasks, timestamp)` or `None`.
    pub fn recall_decomposition(
        &self,
        task_id_or_description: &str,
    ) -> Result<Option<Decomposition>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let q = MemoryQuery {
            kind: Some("task-decomposition".to_string()),
            limit: Some(100),
            ..Default::default()
        };
        let decomps = self.adapter.query(&q)?;
        if decomps.is_empty() {
            return Ok(None);
        }
        // Find by taskId / id-substring first.
        if let Some(d) = decomps
            .iter()
            .find(|d| d.task_id == task_id_or_description || d.id.contains(task_id_or_description))
        {
            return Ok(Some(decomp_from(d)));
        }
        // Fall back to fuzzy token overlap (matches TS).
        let query_tokens = tokenize(&task_id_or_description.to_lowercase());
        let mut best: Option<&MemoryObject> = None;
        let mut best_score = 0.0f64;
        for d in &decomps {
            let desc_tokens = tokenize(&d.description.to_lowercase());
            let overlap = query_tokens
                .iter()
                .filter(|t| desc_tokens.iter().any(|o| o == *t))
                .count();
            let score = overlap as f64 / query_tokens.len().max(1) as f64;
            if score > best_score {
                best_score = score;
                best = Some(d);
            }
        }
        if best_score > 0.2 {
            return Ok(best.map(decomp_from));
        }
        Ok(None)
    }

    pub fn recall(
        &self,
        task_description: &str,
        worker_model: Option<&str>,
    ) -> Result<Option<Recommendation>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let q = MemoryQuery {
            kind: Some("task-observation".to_string()),
            limit: Some(200),
            ..Default::default()
        };
        let objects = self.adapter.query(&q)?;
        if objects.is_empty() {
            return Ok(None);
        }
        let observations: Vec<Observation> = objects.iter().map(obj_to_observation).collect();
        Ok(build_empirical_recommendation(
            task_description,
            &observations,
            worker_model,
        ))
    }

    pub fn write_emission_records(
        &self,
        run_id: &str,
        task_id: &str,
        turn: i64,
        emissions: &[EmittedFileRecord],
    ) -> Result<Vec<String>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let mut ids = Vec::with_capacity(emissions.len());
        let ts = iso_now();
        for (i, e) in emissions.iter().enumerate() {
            let path_hash = sha256_prefix(&e.path, 8);
            let sha256_prefix_str = &e.sha256[..e.sha256.len().min(8)];
            let id = format!("emission-{run_id}-t{turn}-{i}-{path_hash}-{sha256_prefix_str}");
            ids.push(id.clone());

            // Specialized row write (no-op if adapter doesn't support it).
            let row = EmissionRow {
                id: id.clone(),
                run_id: run_id.to_string(),
                task_id: task_id.to_string(),
                turn,
                path: e.path.clone(),
                sha256: e.sha256.clone(),
                bytes: e.bytes,
                before_hash: e.before_hash.clone(),
                existed: e.existed,
                timestamp: e.timestamp.clone().unwrap_or_else(iso_now),
            };
            self.adapter.write_emission_row(&row)?;

            // Generic MemoryObject for back-compat (always written).
            let obj = MemoryObject {
                id: id.clone(),
                kind: "emission".to_string(),
                task_id: task_id.to_string(),
                run_id: Some(run_id.to_string()),
                timestamp: ts.clone(),
                description: format!("Emitted: {}", e.path),
                properties: json!({
                    "runId": run_id,
                    "turn": turn,
                    "path": e.path,
                    "sha256": e.sha256,
                    "bytes": e.bytes,
                    "beforeHash": e.before_hash,
                    "existed": e.existed,
                }),
                tags: vec![
                    "emission".to_string(),
                    if e.existed {
                        "overwrite".into()
                    } else {
                        "create".into()
                    },
                ],
            };
            self.adapter.write(&obj)?;
        }
        Ok(ids)
    }

    pub fn write_run_record(&self, run: &RunRecord) -> Result<()> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let emission_ids = run.emission_ids.clone();

        // Specialized run-row write (no-op if unsupported).
        let row = RunRow::from(run);
        self.adapter.write_run_row(&row)?;

        // Always also write generic MemoryObject for back-compat.
        let obj = MemoryObject {
            id: format!("run-{}", run.run_id),
            kind: "run".to_string(),
            task_id: run.task_id.clone(),
            run_id: Some(run.run_id.clone()),
            timestamp: run.timestamp.clone(),
            description: run.description.clone(),
            properties: json!({
                "language": run.language,
                "taskFamily": run.task_family,
                "mode": run.mode,
                "model": run.model,
                "providerKey": run.provider_key,
                "providerType": run.provider_type,
                "baseUrl": run.base_url,
                "outcome": run.outcome,
                "outcomeClass": run.outcome_class,
                "routingLesson": run.routing_lesson,
                "finalVerdict": run.final_verdict,
                "sourceOfTruth": run.source_of_truth,
                "finalAction": run.final_action,
                "tokens": run.tokens,
                "durationMs": run.duration_ms,
                "turns": run.turns,
                "validatorDurationMs": run.validator_duration_ms,
                "verifierOverall": run.verifier_overall,
                "filesEmitted": run.files_emitted,
                "totalBytesEmitted": run.total_bytes_emitted,
                "emissionCount": emission_ids.len(),
                "emissionIds": emission_ids,
            }),
            tags: vec![
                "run".to_string(),
                run.outcome_class.clone(),
                run.routing_lesson.clone(),
            ],
        };
        self.adapter.write(&obj)
    }

    /// Transactional write of run + emissions. Delegates to the adapter's
    /// transactional path when available; falls back to sequential writes.
    pub fn write_run_and_emissions(
        &self,
        run: &mut RunRecord,
        emissions: &[EmittedFileRecord],
        turn: i64,
    ) -> Result<()> {
        let _g = self.lock.lock().expect("store lock poisoned");
        // Compute emission ids up-front.
        let ids: Vec<String> = emissions
            .iter()
            .enumerate()
            .map(|(i, e)| {
                e.id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        run.run_id,
                        e.path,
                        &e.sha256[..e.sha256.len().min(12)]
                    )
                    .replace('{', "")
                    .replace('}', "")
                        + &format!("-{i}")
                })
            })
            .collect();
        run.emission_ids = ids.clone();
        run.files_emitted = emissions.len() as i64;
        run.total_bytes_emitted = emissions.iter().map(|e| e.bytes).sum();

        let row = RunRow::from(&*run);
        let emission_rows: Vec<EmissionRow> = emissions
            .iter()
            .enumerate()
            .map(|(i, e)| EmissionRow {
                id: ids[i].clone(),
                run_id: run.run_id.clone(),
                task_id: run.task_id.clone(),
                turn,
                path: e.path.clone(),
                sha256: e.sha256.clone(),
                bytes: e.bytes,
                before_hash: e.before_hash.clone(),
                existed: e.existed,
                timestamp: e.timestamp.clone().unwrap_or_else(iso_now),
            })
            .collect();
        if self
            .adapter
            .write_run_and_emissions_tx(&row, &emission_rows)?
        {
            return Ok(());
        }
        // Fallback: sequential writes.
        let _ = self.write_emission_records(&run.run_id, &run.task_id, turn, emissions)?;
        self.write_run_record(run)
    }

    pub fn query_runs(&self, limit: Option<usize>) -> Result<Vec<MemoryObject>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        if let Some(rows) = self.adapter.query_run_rows(limit.unwrap_or(50))? {
            Ok(rows.iter().map(run_row_to_object).collect())
        } else {
            let q = MemoryQuery {
                kind: Some("run".to_string()),
                limit: Some(limit.unwrap_or(50)),
                ..Default::default()
            };
            self.adapter.query(&q)
        }
    }

    pub fn query_emissions(&self, task_id: &str) -> Result<Vec<MemoryObject>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        let q = MemoryQuery {
            kind: Some("emission".to_string()),
            limit: Some(1000),
            ..Default::default()
        };
        let all = self.adapter.query(&q)?;
        Ok(all.into_iter().filter(|o| o.task_id == task_id).collect())
    }

    pub fn query_emissions_for_run(&self, run_id: &str) -> Result<Vec<MemoryObject>> {
        let _g = self.lock.lock().expect("store lock poisoned");
        if let Some(rows) = self.adapter.query_emission_rows_for_run(run_id)? {
            Ok(rows.iter().map(emission_row_to_object).collect())
        } else {
            let q = MemoryQuery {
                kind: Some("emission".to_string()),
                limit: Some(1000),
                ..Default::default()
            };
            let all = self.adapter.query(&q)?;
            Ok(all
                .into_iter()
                .filter(|o| {
                    o.properties
                        .get("runId")
                        .and_then(|v| v.as_str())
                        .is_some_and(|r| r == run_id)
                })
                .collect())
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decomposition {
    pub task_id: String,
    pub description: String,
    pub tasks: Vec<Value>,
    pub timestamp: String,
}

fn decomp_from(d: &MemoryObject) -> Decomposition {
    let tasks = d
        .properties
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Decomposition {
        task_id: d.task_id.clone(),
        description: d.description.clone(),
        tasks,
        timestamp: d.timestamp.clone(),
    }
}

fn obj_to_observation(o: &MemoryObject) -> Observation {
    let gstr = |key: &str| {
        o.properties
            .get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    let gnum = |key: &str| o.properties.get(key).and_then(|v| v.as_f64());
    let vector = o
        .properties
        .get("vector")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect::<Vec<u32>>()
        });
    Observation {
        description: o.description.clone(),
        vector,
        language: gstr("language"),
        task_family: gstr("taskFamily"),
        mode: gstr("mode"),
        model: gstr("model"),
        outcome: gstr("outcome"),
        source_of_truth: gstr("sourceOfTruth"),
        routing_lesson: gstr("routingLesson"),
        outcome_class: gstr("outcomeClass"),
        reason: gstr("reason"),
        tokens: gnum("tokens"),
        duration_ms: gnum("durationMs"),
    }
}

fn run_row_to_object(r: &RunRow) -> MemoryObject {
    MemoryObject {
        id: format!("run-{}", r.run_id),
        kind: "run".to_string(),
        task_id: r.task_id.clone(),
        run_id: Some(r.run_id.clone()),
        timestamp: r.timestamp.clone(),
        description: r.description.clone(),
        properties: json!({
            "language": r.language,
            "taskFamily": r.task_family,
            "mode": r.mode,
            "model": r.model,
            "providerKey": r.provider_key,
            "providerType": r.provider_type,
            "baseUrl": r.base_url,
            "outcome": r.outcome,
            "outcomeClass": r.outcome_class,
            "routingLesson": r.routing_lesson,
            "finalVerdict": r.final_verdict,
            "sourceOfTruth": r.source_of_truth,
            "finalAction": r.final_action,
            "tokens": r.tokens,
            "durationMs": r.duration_ms,
            "turns": r.turns,
            "validatorDurationMs": r.validator_duration_ms,
            "verifierOverall": r.verifier_overall,
            "filesEmitted": r.files_emitted,
            "totalBytesEmitted": r.total_bytes_emitted,
            "emissionCount": r.emission_ids.len(),
            "emissionIds": r.emission_ids,
        }),
        tags: vec!["run".to_string()],
    }
}

fn emission_row_to_object(r: &EmissionRow) -> MemoryObject {
    MemoryObject {
        id: r.id.clone(),
        kind: "emission".to_string(),
        task_id: r.task_id.clone(),
        run_id: Some(r.run_id.clone()),
        timestamp: r.timestamp.clone(),
        description: format!("Emitted: {}", r.path),
        properties: json!({
            "runId": r.run_id,
            "path": r.path,
            "sha256": r.sha256,
            "bytes": r.bytes,
            "beforeHash": r.before_hash,
            "existed": r.existed,
        }),
        tags: vec!["emission".to_string()],
    }
}

fn sha256_prefix(s: &str, n: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let full = hex::encode(hasher.finalize());
    full[..full.len().min(n)].to_string()
}
