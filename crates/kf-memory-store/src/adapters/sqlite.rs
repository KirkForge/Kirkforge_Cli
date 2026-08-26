//! SQLite-backed memory adapter. Port of
//! `memory-palace/src/sqlite-adapter.ts`. Uses `rusqlite` (the Rust
//! equivalent of `better-sqlite3`).
//!
//! Ports the schema DDL, `SCHEMA_VERSION = 3`, prepared statements
//! (re-prepared per call — rusqlite statements are tied to the borrowed
//! Connection lifetime), migrations (v2: outcome_reason, v3: routing_bias),
//! and backup/restore.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::adapters::MemoryAdapter;
use crate::time::{iso_now, now_millis};
use crate::types::{
    BackupMetadata, BackupRowCount, EmissionRow, MemoryObject, MemoryQuery, MemoryStats, RunRow,
};

/// Current schema version. Increment when adding migrations.
pub const SCHEMA_VERSION: i64 = 3;

pub struct SqliteAdapter {
    conn: Mutex<Connection>,
    file_path: PathBuf,
}

impl SqliteAdapter {
    pub fn open<P: Into<PathBuf>>(path: P) -> Result<Self> {
        let file_path = path.into();
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&file_path)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            file_path,
        })
    }

    /// In-memory database (for tests / ephemeral stores).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            file_path: PathBuf::from(":memory:"),
        })
    }

    pub fn close(self) -> Result<()> {
        self.conn
            .into_inner()
            .map_err(|_| anyhow!("sqlite mutex poisoned"))?
            .close()
            .map_err(|(_, e)| anyhow!("close failed: {e}"))?;
        Ok(())
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS observations (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                task_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                description TEXT NOT NULL,
                properties TEXT NOT NULL,
                tags TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_obs_kind ON observations(kind);
            CREATE INDEX IF NOT EXISTS idx_obs_task_id ON observations(task_id);
            CREATE INDEX IF NOT EXISTS idx_obs_timestamp ON observations(timestamp);
            CREATE INDEX IF NOT EXISTS idx_obs_tags ON observations(tags);

            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                description TEXT NOT NULL,
                language TEXT NOT NULL,
                task_family TEXT,
                mode TEXT NOT NULL,
                model TEXT NOT NULL,
                provider_key TEXT NOT NULL DEFAULT '',
                provider_type TEXT NOT NULL DEFAULT '',
                base_url TEXT,
                outcome TEXT NOT NULL,
                outcome_class TEXT NOT NULL,
                routing_lesson TEXT NOT NULL DEFAULT 'neutral',
                final_verdict TEXT NOT NULL,
                source_of_truth TEXT NOT NULL,
                final_action TEXT NOT NULL,
                tokens INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                turns INTEGER NOT NULL DEFAULT 0,
                validator_duration_ms INTEGER NOT NULL DEFAULT 0,
                verifier_overall TEXT,
                files_emitted INTEGER NOT NULL DEFAULT 0,
                total_bytes_emitted INTEGER NOT NULL DEFAULT 0,
                emission_ids TEXT NOT NULL DEFAULT '[]',
                timestamp TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_runs_task_id ON runs(task_id);
            CREATE INDEX IF NOT EXISTS idx_runs_model ON runs(model);
            CREATE INDEX IF NOT EXISTS idx_runs_outcome_class ON runs(outcome_class);
            CREATE INDEX IF NOT EXISTS idx_runs_timestamp ON runs(timestamp);

            CREATE TABLE IF NOT EXISTS emissions (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                turn INTEGER NOT NULL DEFAULT 0,
                path TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                bytes INTEGER NOT NULL DEFAULT 0,
                before_hash TEXT,
                existed INTEGER NOT NULL DEFAULT 0,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (run_id) REFERENCES runs(run_id)
            );
            CREATE INDEX IF NOT EXISTS idx_emissions_task_id ON emissions(task_id);
            CREATE INDEX IF NOT EXISTS idx_emissions_run_id ON emissions(run_id);

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )?;

        // Seed migration 1 if empty.
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if current == 0 {
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params![1, iso_now()],
            )?;
            Self::run_migrations(conn, 1)?;
        } else {
            Self::run_migrations(conn, current)?;
        }
        Ok(())
    }

    fn run_migrations(conn: &Connection, from_version: i64) -> Result<()> {
        // Migration 2: add outcome_reason to runs.
        if from_version < 2 {
            let has_col = column_exists(conn, "runs", "outcome_reason")?;
            if !has_col {
                conn.execute("ALTER TABLE runs ADD COLUMN outcome_reason TEXT", [])?;
            }
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params![2, iso_now()],
            )?;
        }
        // Migration 3: add routing_bias to observations.
        if from_version < 3 {
            let has_col = column_exists(conn, "observations", "routing_bias")?;
            if !has_col {
                conn.execute("ALTER TABLE observations ADD COLUMN routing_bias TEXT", [])?;
            }
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params![3, iso_now()],
            )?;
        }
        Ok(())
    }

    fn row_to_object(row: &rusqlite::Row) -> rusqlite::Result<MemoryObject> {
        let props_str: String = row.get("properties")?;
        let tags_str: String = row.get("tags")?;
        Ok(MemoryObject {
            id: row.get("id")?,
            kind: row.get("kind")?,
            task_id: row.get("task_id")?,
            run_id: None,
            timestamp: row.get("timestamp")?,
            description: row.get("description")?,
            properties: serde_json::from_str(&props_str).unwrap_or(Value::Null),
            tags: serde_json::from_str(&tags_str).unwrap_or_default(),
        })
    }

    pub fn backup<P: Into<PathBuf>>(&self, dest_path: Option<P>) -> Result<BackupMetadata> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        let timestamp_file = iso_now_ms().replace([':', '.'], "-");
        let backup_path = match dest_path {
            Some(p) => p.into(),
            None => {
                let mut s = self.file_path.as_os_str().to_owned();
                s.push(format!(".backup.{timestamp_file}"));
                PathBuf::from(s)
            }
        };
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)?;
        }
        // rusqlite backup API: copy main → target connection.
        {
            let mut target = Connection::open(&backup_path)?;
            let backup = rusqlite::backup::Backup::new(&conn, &mut target)?;
            backup.run_to_completion(100, std::time::Duration::from_millis(0), None)?;
        }

        let file_contents = fs::read(&backup_path)?;
        let sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&file_contents);
            hex::encode(hasher.finalize())
        };
        let row_count = count_rows(&conn)?;
        Ok(BackupMetadata {
            file_path: backup_path.to_string_lossy().into_owned(),
            size_bytes: file_contents.len() as u64,
            sha256,
            schema_version: Some(Self::schema_version_locked(&conn)),
            timestamp: iso_now(),
            row_count,
        })
    }

    pub fn restore<P: AsRef<Path>>(&self, backup_path: P) -> Result<BackupMetadata> {
        let source_path = backup_path.as_ref();
        if !source_path.exists() {
            return Err(anyhow!("Backup file not found: {}", source_path.display()));
        }
        let file_contents = fs::read(source_path)?;
        let sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(&file_contents);
            hex::encode(hasher.finalize())
        };
        let mut conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.execute_batch("COMMIT;").ok();
        // Replace the on-disk file then reopen.
        fs::copy(source_path, &self.file_path)?;
        let reopened = Connection::open(&self.file_path)?;
        Self::init_schema(&reopened)?;
        *conn = reopened;
        let row_count = count_rows(&conn)?;
        Ok(BackupMetadata {
            file_path: source_path.to_string_lossy().into_owned(),
            size_bytes: file_contents.len() as u64,
            sha256,
            schema_version: Some(Self::schema_version_locked(&conn)),
            timestamp: iso_now(),
            row_count,
        })
    }

    pub fn list_backups(&self, directory: Option<&Path>) -> Vec<PathBuf> {
        let dir = directory
            .map(PathBuf::from)
            .or_else(|| self.file_path.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let base = self
            .file_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let prefix = format!("{base}.backup.");
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let name = name.to_str()?;
                name.starts_with(&prefix).then(|| e.path())
            })
            .collect();
        out.sort();
        out
    }

    fn schema_version_locked(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COALESCE(MAX(version), 1) FROM schema_migrations",
            [],
            |r| r.get(0),
        )
        .unwrap_or(SCHEMA_VERSION)
    }
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    Ok(names.iter().any(|n| n == column))
}

fn count_rows(conn: &Connection) -> Result<BackupRowCount> {
    let count = |table: &str| -> Result<i64> {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    };
    Ok(BackupRowCount {
        observations: count("observations")?,
        runs: count("runs")?,
        emissions: count("emissions")?,
    })
}

/// ISO timestamp with millisecond precision. Backup filenames need this so
/// two backups in the same second don't collide (matches TS `toISOString()`
/// which emits `YYYY-MM-DDTHH:MM:SS.mmmZ`).
fn iso_now_ms() -> String {
    let ms = now_millis();
    let secs = ms / 1000;
    let millis = ms % 1000;
    let (y, mo, d, h, mi, s) = crate::time::unix_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

impl MemoryAdapter for SqliteAdapter {
    fn write(&self, obj: &MemoryObject) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO observations
             (id, kind, task_id, timestamp, description, properties, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                obj.id,
                obj.kind,
                obj.task_id,
                obj.timestamp,
                obj.description,
                serde_json::to_string(&obj.properties)?,
                serde_json::to_string(&obj.tags)?,
            ],
        )?;
        Ok(())
    }

    fn read(&self, id: &str) -> Result<Option<MemoryObject>> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, kind, task_id, timestamp, description, properties, tags
             FROM observations WHERE id = ?1",
        )?;
        let obj = match stmt.query_row(params![id], Self::row_to_object) {
            Ok(o) => Some(o),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(obj)
    }

    fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.execute("DELETE FROM observations WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryObject>> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        let mut conditions: Vec<String> = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(kind) = &q.kind {
            conditions.push("kind = ?".to_string());
            params_vec.push(Box::new(kind.clone()));
        }
        if let Some(since) = &q.since {
            conditions.push("timestamp >= ?".to_string());
            params_vec.push(Box::new(since.clone()));
        }
        if let Some(tags) = &q.tags {
            for tag in tags {
                conditions.push("tags LIKE ?".to_string());
                params_vec.push(Box::new(format!("%\"{tag}\"%")));
            }
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let limit = q.limit.unwrap_or(1000);
        let sql = format!(
            "SELECT id, kind, task_id, timestamp, description, properties, tags
             FROM observations {where_clause} ORDER BY timestamp DESC LIMIT ?"
        );
        params_vec.push(Box::new(limit as i64));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_object)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn stats(&self) -> Result<MemoryStats> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?;
        let last: Option<String> = match conn.query_row(
            "SELECT timestamp FROM observations ORDER BY timestamp DESC LIMIT 1",
            [],
            |r| r.get(0),
        ) {
            Ok(s) => Some(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(MemoryStats {
            total_objects: count as usize,
            last_write: last.unwrap_or_else(|| "never".to_string()),
        })
    }

    fn write_run_row(&self, run: &RunRow) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO runs
             (run_id, task_id, description, language, task_family, mode, model,
              provider_key, provider_type, base_url, outcome, outcome_class,
              routing_lesson, final_verdict, source_of_truth, final_action,
              tokens, duration_ms, turns, validator_duration_ms, verifier_overall,
              files_emitted, total_bytes_emitted, emission_ids, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                run.run_id,
                run.task_id,
                run.description,
                run.language,
                run.task_family,
                run.mode,
                run.model,
                run.provider_key,
                run.provider_type,
                run.base_url,
                run.outcome,
                run.outcome_class,
                run.routing_lesson,
                run.final_verdict,
                run.source_of_truth,
                run.final_action,
                run.tokens,
                run.duration_ms,
                run.turns,
                run.validator_duration_ms,
                run.verifier_overall,
                run.files_emitted,
                run.total_bytes_emitted,
                serde_json::to_string(&run.emission_ids)?,
                run.timestamp,
            ],
        )?;
        Ok(())
    }

    fn write_emission_row(&self, emission: &EmissionRow) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO emissions
             (id, run_id, task_id, turn, path, sha256, bytes, before_hash, existed, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                emission.id,
                emission.run_id,
                emission.task_id,
                emission.turn,
                emission.path,
                emission.sha256,
                emission.bytes,
                emission.before_hash,
                emission.existed as i64,
                emission.timestamp,
            ],
        )?;
        Ok(())
    }

    fn write_run_and_emissions_tx(&self, run: &RunRow, emissions: &[EmissionRow]) -> Result<bool> {
        let mut conn = self.conn.lock().expect("sqlite lock poisoned");
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO runs
             (run_id, task_id, description, language, task_family, mode, model,
              provider_key, provider_type, base_url, outcome, outcome_class,
              routing_lesson, final_verdict, source_of_truth, final_action,
              tokens, duration_ms, turns, validator_duration_ms, verifier_overall,
              files_emitted, total_bytes_emitted, emission_ids, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                run.run_id,
                run.task_id,
                run.description,
                run.language,
                run.task_family,
                run.mode,
                run.model,
                run.provider_key,
                run.provider_type,
                run.base_url,
                run.outcome,
                run.outcome_class,
                run.routing_lesson,
                run.final_verdict,
                run.source_of_truth,
                run.final_action,
                run.tokens,
                run.duration_ms,
                run.turns,
                run.validator_duration_ms,
                run.verifier_overall,
                run.files_emitted,
                run.total_bytes_emitted,
                serde_json::to_string(&run.emission_ids)?,
                run.timestamp,
            ],
        )?;
        tx.execute(
            "DELETE FROM emissions WHERE run_id = ?1",
            params![run.run_id],
        )?;
        for e in emissions {
            tx.execute(
                "INSERT OR REPLACE INTO emissions
                 (id, run_id, task_id, turn, path, sha256, bytes, before_hash, existed, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    e.id,
                    e.run_id,
                    e.task_id,
                    e.turn,
                    e.path,
                    e.sha256,
                    e.bytes,
                    e.before_hash,
                    e.existed as i64,
                    e.timestamp,
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    fn query_run_rows(&self, limit: usize) -> Result<Option<Vec<RunRow>>> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT run_id, task_id, description, language, task_family, mode, model,
                    provider_key, provider_type, base_url, outcome, outcome_class,
                    routing_lesson, final_verdict, source_of_truth, final_action,
                    tokens, duration_ms, turns, validator_duration_ms, verifier_overall,
                    files_emitted, total_bytes_emitted, emission_ids, timestamp
             FROM runs ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let emission_ids_str: String = r.get("emission_ids")?;
            Ok(RunRow {
                run_id: r.get("run_id")?,
                task_id: r.get("task_id")?,
                description: r.get("description")?,
                language: r.get("language")?,
                task_family: r.get("task_family")?,
                mode: r.get("mode")?,
                model: r.get("model")?,
                provider_key: r.get("provider_key")?,
                provider_type: r.get("provider_type")?,
                base_url: r.get("base_url")?,
                outcome: r.get("outcome")?,
                outcome_class: r.get("outcome_class")?,
                routing_lesson: r.get("routing_lesson")?,
                final_verdict: r.get("final_verdict")?,
                source_of_truth: r.get("source_of_truth")?,
                final_action: r.get("final_action")?,
                tokens: r.get("tokens")?,
                duration_ms: r.get("duration_ms")?,
                turns: r.get("turns")?,
                validator_duration_ms: r.get("validator_duration_ms")?,
                verifier_overall: r.get("verifier_overall")?,
                files_emitted: r.get("files_emitted")?,
                total_bytes_emitted: r.get("total_bytes_emitted")?,
                emission_ids: serde_json::from_str(&emission_ids_str).unwrap_or_default(),
                timestamp: r.get("timestamp")?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Some(out))
    }

    fn query_emission_rows_for_run(&self, run_id: &str) -> Result<Option<Vec<EmissionRow>>> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, run_id, task_id, turn, path, sha256, bytes, before_hash, existed, timestamp
             FROM emissions WHERE run_id = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            let existed: i64 = r.get("existed")?;
            Ok(EmissionRow {
                id: r.get("id")?,
                run_id: r.get("run_id")?,
                task_id: r.get("task_id")?,
                turn: r.get("turn")?,
                path: r.get("path")?,
                sha256: r.get("sha256")?,
                bytes: r.get("bytes")?,
                before_hash: r.get("before_hash")?,
                existed: existed != 0,
                timestamp: r.get("timestamp")?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Some(out))
    }

    fn schema_version(&self) -> Option<i64> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        Some(Self::schema_version_locked(&conn))
    }

    fn persist(&self) -> Result<()> {
        let conn = self.conn.lock().expect("sqlite lock poisoned");
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(())
    }
}
