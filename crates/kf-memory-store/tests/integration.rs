//! Round-trip + semantic parity tests for the three adapters and the
//! `MemoryStore` facade. Mirrors the TS test surface for memory-palace.

use std::sync::Arc;

use kf_memory_store::{
    FileAdapter, InMemoryAdapter, MemoryAdapter, MemoryObject, MemoryQuery, MemoryStore,
    MemoryStoreOptions, SqliteAdapter, TaskObservationInput,
};
use serde_json::json;

fn obj(id: &str, kind: &str, ts: &str) -> MemoryObject {
    MemoryObject {
        id: id.to_string(),
        kind: kind.to_string(),
        task_id: "T1".to_string(),
        run_id: None,
        timestamp: ts.to_string(),
        description: format!("desc {id}"),
        properties: json!({"language": "python"}),
        tags: vec!["t1".to_string(), "python".to_string()],
    }
}

// ── InMemoryAdapter ────────────────────────────────────────────────────────

#[test]
fn in_memory_write_read_roundtrip() {
    let a = InMemoryAdapter::new();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    let got = a.read("a").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
    assert_eq!(got.description, "desc a");
}

#[test]
fn in_memory_query_filters_by_kind_and_sorts_desc() {
    let a = InMemoryAdapter::new();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    a.write(&obj("c", "run", "2024-03-01T00:00:00Z")).unwrap();
    let q = MemoryQuery {
        kind: Some("obs".into()),
        ..Default::default()
    };
    let results = a.query(&q).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "b"); // newest first
}

#[test]
fn in_memory_query_tag_and_since_filters() {
    let a = InMemoryAdapter::new();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-06-01T00:00:00Z")).unwrap();
    let q = MemoryQuery {
        since: Some("2024-05-01T00:00:00Z".into()),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 1);
    let q = MemoryQuery {
        tags: Some(vec!["python".into()]),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 2);
    let q = MemoryQuery {
        tags: Some(vec!["nope".into()]),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 0);
}

#[test]
fn in_memory_query_respects_limit() {
    let a = InMemoryAdapter::new();
    for i in 0..10 {
        a.write(&obj(&format!("o{i}"), "obs", &format!("2024-01-{i:02}")))
            .unwrap();
    }
    let q = MemoryQuery {
        limit: Some(3),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 3);
}

#[test]
fn in_memory_stats_reports_count_and_last_write() {
    let a = InMemoryAdapter::new();
    let s = a.stats().unwrap();
    assert_eq!(s.total_objects, 0);
    assert_eq!(s.last_write, "never");
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    let s = a.stats().unwrap();
    assert_eq!(s.total_objects, 1);
    assert_eq!(s.last_write, "2024-01-01T00:00:00Z");
}

#[test]
fn in_memory_delete_removes_entry() {
    let a = InMemoryAdapter::new();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    a.delete("a").unwrap();
    assert!(a.read("a").unwrap().is_none());
    assert_eq!(a.stats().unwrap().total_objects, 1);
    // Deleting a missing id is a no-op, not an error.
    a.delete("nope").unwrap();
    assert_eq!(a.stats().unwrap().total_objects, 1);
}

// ── FileAdapter ────────────────────────────────────────────────────────────

#[test]
fn file_adapter_roundtrips_json_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mem.json");
    let a = FileAdapter::new(&path);
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.persist().unwrap();
    // Re-open and verify persistence.
    let b = FileAdapter::new(&path);
    let got = b.read("a").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
}

#[test]
fn file_adapter_corrupt_file_backed_up_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mem.json");
    std::fs::write(&path, b"NOT JSON").unwrap();
    let a = FileAdapter::new(&path);
    let err = a.stats().unwrap_err();
    assert!(err.to_string().contains("unusable"));
    // .corrupt backup should exist.
    let corrupt = {
        let mut s = path.as_os_str().to_owned();
        s.push(".corrupt");
        s
    };
    assert!(std::path::Path::new(&corrupt).exists());
}

#[test]
fn file_adapter_missing_file_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nope.json");
    let a = FileAdapter::new(&path);
    let s = a.stats().unwrap();
    assert_eq!(s.total_objects, 0);
    assert_eq!(s.last_write, "never");
}

#[test]
fn file_adapter_query_filters_match_in_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let a = FileAdapter::new(tmp.path().join("mem.json"));
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    a.write(&obj("c", "run", "2024-03-01T00:00:00Z")).unwrap();
    let q = MemoryQuery {
        kind: Some("obs".into()),
        ..Default::default()
    };
    let results = a.query(&q).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "b");
}

#[test]
fn file_adapter_reclaims_stale_lock_from_dead_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mem.json");
    let lock_path = {
        let mut s = path.as_os_str().to_owned();
        s.push(".lock");
        std::path::PathBuf::from(s)
    };
    // Simulate a crashed process: write a stale PID (999999 almost certainly
    // dead) to the lock file so the liveness check reclaims it.
    std::fs::write(&lock_path, "999999\n").unwrap();
    let a = FileAdapter::new(&path);
    // write() must succeed — the stale lock is reclaimed, not waited on.
    // (LockGuard::Drop removes the lock file when write() returns, so the
    // lock file no longer exists after this call.)
    a.write(&obj("recovered", "obs", "2024-01-01T00:00:00Z"))
        .unwrap();
    // The store is usable after recovery.
    let got = a.read("recovered").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
    // The stale lock file was consumed — a fresh write works without manual
    // cleanup (proving the reclaim, not just a retry-tolerant write).
    a.write(&obj("second", "run", "2024-02-01T00:00:00Z"))
        .unwrap();
    assert_eq!(a.stats().unwrap().total_objects, 2);
}

#[test]
fn file_adapter_reclaims_lock_with_unreadable_pid_via_age_fallback() {
    // When the PID can't be parsed (garbage in the lock file), the age
    // fallback should reclaim it. We make the lock file old by setting its
    // mtime far in the past so the 5-min staleness threshold is met.
    use std::time::{Duration, SystemTime};
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mem.json");
    let lock_path = {
        let mut s = path.as_os_str().to_owned();
        s.push(".lock");
        std::path::PathBuf::from(s)
    };
    std::fs::write(&lock_path, "not-a-number\n").unwrap();
    // Set mtime to 10 minutes ago — past the 5-min staleness threshold.
    // Open with write access: on Windows, SetFileTime needs
    // FILE_WRITE_ATTRIBUTES, which a read-only File::open handle lacks.
    let old = SystemTime::now() - Duration::from_secs(600);
    let times = std::fs::FileTimes::new().set_modified(old);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .unwrap()
        .set_times(times)
        .unwrap();
    let a = FileAdapter::new(&path);
    a.write(&obj("recovered2", "obs", "2024-01-01T00:00:00Z"))
        .unwrap();
    let got = a.read("recovered2").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
}

#[test]
fn file_adapter_delete_removes_and_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mem.json");
    let a = FileAdapter::new(&path);
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    a.delete("a").unwrap();
    assert!(a.read("a").unwrap().is_none());
    assert_eq!(a.stats().unwrap().total_objects, 1);
    // Re-open from the same file: the deletion must have been flushed.
    let b = FileAdapter::new(&path);
    assert!(b.read("a").unwrap().is_none());
    assert_eq!(b.stats().unwrap().total_objects, 1);
}

// ── SqliteAdapter ──────────────────────────────────────────────────────────

#[test]
fn sqlite_open_in_memory_initializes_schema() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    assert_eq!(a.schema_version(), Some(3));
    let s = a.stats().unwrap();
    assert_eq!(s.total_objects, 0);
}

#[test]
fn sqlite_write_read_roundtrip() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    let got = a.read("a").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
    assert_eq!(got.tags, vec!["t1".to_string(), "python".to_string()]);
}

#[test]
fn sqlite_delete_removes_entry() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    a.delete("a").unwrap();
    assert!(a.read("a").unwrap().is_none());
    assert_eq!(a.stats().unwrap().total_objects, 1);
    // Deleting a missing id is a no-op, not an error.
    a.delete("nope").unwrap();
    assert_eq!(a.stats().unwrap().total_objects, 1);
}

#[test]
fn sqlite_query_kind_filter_and_limit() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    for i in 0..5 {
        a.write(&obj(&format!("o{i}"), "obs", &format!("2024-01-0{i}")))
            .unwrap();
    }
    a.write(&obj("r1", "run", "2024-12-01")).unwrap();
    let q = MemoryQuery {
        kind: Some("obs".into()),
        limit: Some(2),
        ..Default::default()
    };
    let r = a.query(&q).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].id, "o4");
}

#[test]
fn sqlite_query_tag_uses_like_match() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    a.write(&obj("a", "obs", "2024-01-01")).unwrap();
    let q = MemoryQuery {
        tags: Some(vec!["python".into()]),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 1);
    let q = MemoryQuery {
        tags: Some(vec!["java".into()]),
        ..Default::default()
    };
    assert_eq!(a.query(&q).unwrap().len(), 0);
}

#[test]
fn sqlite_stats_reports_count_and_last_write() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    a.write(&obj("a", "obs", "2024-01-01T00:00:00Z")).unwrap();
    a.write(&obj("b", "obs", "2024-02-01T00:00:00Z")).unwrap();
    let s = a.stats().unwrap();
    assert_eq!(s.total_objects, 2);
    assert_eq!(s.last_write, "2024-02-01T00:00:00Z");
}

#[test]
fn sqlite_schema_version_is_3_after_init() {
    let a = SqliteAdapter::open_in_memory().unwrap();
    assert_eq!(a.schema_version(), Some(3));
}

#[test]
fn sqlite_backup_then_restore_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("db.sqlite");
    let bak_path = tmp.path().join("db.bak");
    {
        let a = SqliteAdapter::open(&db_path).unwrap();
        a.write(&obj("a", "obs", "2024-01-01")).unwrap();
        a.write(&obj("b", "obs", "2024-02-01")).unwrap();
        let md = a.backup(Some(&bak_path)).unwrap();
        assert_eq!(md.row_count.observations, 2);
        assert_eq!(md.row_count.runs, 0);
        assert_eq!(md.schema_version, Some(3));
    }
    // Restore into a fresh adapter at the same path.
    let restored = SqliteAdapter::open(&db_path).unwrap();
    restored.restore(&bak_path).unwrap();
    assert_eq!(restored.stats().unwrap().total_objects, 2);
    let got = restored.read("a").unwrap().unwrap();
    assert_eq!(got.kind, "obs");
}

#[test]
fn sqlite_list_backups_finds_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("db.sqlite");
    let a = SqliteAdapter::open(&db_path).unwrap();
    a.write(&obj("a", "obs", "2024-01-01")).unwrap();
    a.backup::<&std::path::Path>(None).unwrap();
    a.backup::<&std::path::Path>(None).unwrap();
    let backups = a.list_backups(None);
    assert_eq!(backups.len(), 2, "expected 2 backups, got {backups:?}");
}

// ── MemoryStore facade ─────────────────────────────────────────────────────

fn store_with(adapter: impl MemoryAdapter + 'static) -> MemoryStore {
    MemoryStore::new(adapter, MemoryStoreOptions::default())
}

#[test]
fn store_write_task_observation_inserts_and_tags() {
    let store = store_with(InMemoryAdapter::new());
    let input = TaskObservationInput {
        task_id: "T1".into(),
        description: "build a web server endpoint".into(),
        language: "python".into(),
        mode: "artifact".into(),
        model: "qwen".into(),
        task_pass: Some(true),
        tokens: 100,
        duration_ms: 5000,
        ..Default::default()
    };
    store.write_task_observation(&input).unwrap();
    let q = MemoryQuery {
        kind: Some("task-observation".into()),
        ..Default::default()
    };
    let obs = store.adapter().query(&q).unwrap();
    assert_eq!(obs.len(), 1);
    assert!(obs[0].tags.contains(&"pass".to_string()));
    assert_eq!(obs[0].properties["taskFamily"].as_str().unwrap(), "web");
    // vector should be a non-empty array.
    let vec = obs[0].properties["vector"].as_array().unwrap();
    assert_eq!(vec.len(), 64);
}

#[test]
fn store_write_task_observation_infers_outcome_from_task_pass() {
    let store = store_with(InMemoryAdapter::new());
    let mut input = TaskObservationInput {
        task_id: "T1".into(),
        description: "do thing".into(),
        language: "python".into(),
        mode: "artifact".into(),
        model: "qwen".into(),
        tokens: 100,
        duration_ms: 0,
        ..Default::default()
    };
    input.task_pass = Some(false);
    store.write_task_observation(&input).unwrap();
    let q = MemoryQuery {
        kind: Some("task-observation".into()),
        ..Default::default()
    };
    let obs = store.adapter().query(&q).unwrap();
    assert_eq!(obs[0].properties["outcome"].as_str().unwrap(), "fail");
}

#[test]
fn store_recall_returns_recommendation_for_similar_obs() {
    let store = store_with(InMemoryAdapter::new());
    // Seed a passing observation.
    let seed = TaskObservationInput {
        task_id: "T0".into(),
        description: "write a python module".into(),
        language: "python".into(),
        mode: "artifact".into(),
        model: "good-model".into(),
        task_pass: Some(true),
        tokens: 100,
        duration_ms: 0,
        ..Default::default()
    };
    store.write_task_observation(&seed).unwrap();
    let rec = store.recall("write a python module", None).unwrap();
    assert!(rec.is_some(), "recall should find a recommendation");
    let r = rec.unwrap();
    assert_eq!(r.model, "good-model");
    assert!(r.routing_bias.prefer.contains(&"good-model".to_string()));
}

#[test]
fn store_recall_returns_none_when_empty() {
    let store = store_with(InMemoryAdapter::new());
    assert!(store.recall("anything", None).unwrap().is_none());
}

#[test]
fn store_write_decomposition_and_recall_by_id() {
    let store = store_with(InMemoryAdapter::new());
    let tasks = vec![json!({"id": "sub1"}), json!({"id": "sub2"})];
    store
        .write_decomposition("T1", "build the thing", &tasks, "python")
        .unwrap();
    let recalled = store.recall_decomposition("T1").unwrap().unwrap();
    assert_eq!(recalled.task_id, "T1");
    assert_eq!(recalled.description, "build the thing");
    assert_eq!(recalled.tasks.len(), 2);
}

#[test]
fn store_recall_decomposition_fuzzy_match() {
    let store = store_with(InMemoryAdapter::new());
    let tasks = vec![json!({"id": "x"})];
    store
        .write_decomposition("T1", "build the web server", &tasks, "python")
        .unwrap();
    let recalled = store
        .recall_decomposition("web server build")
        .unwrap()
        .unwrap();
    assert_eq!(recalled.task_id, "T1");
}

#[test]
fn store_recall_decomposition_returns_none_when_no_match() {
    let store = store_with(InMemoryAdapter::new());
    let tasks = vec![json!({"id": "x"})];
    store
        .write_decomposition("T1", "build the web server", &tasks, "python")
        .unwrap();
    let recalled = store.recall_decomposition("zzzzz").unwrap();
    assert!(recalled.is_none());
}

#[test]
fn store_write_run_record_writes_generic_object() {
    let store = store_with(InMemoryAdapter::new());
    let run = sample_run("R1", "T1");
    store.write_run_record(&run).unwrap();
    let q = MemoryQuery {
        kind: Some("run".into()),
        ..Default::default()
    };
    let runs = store.adapter().query(&q).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, "run-R1");
}

#[test]
fn store_write_run_record_with_sqlite_writes_specialized_row() {
    let store = store_with(SqliteAdapter::open_in_memory().unwrap());
    let run = sample_run("R1", "T1");
    store.write_run_record(&run).unwrap();
    let runs = store.query_runs(None).unwrap();
    assert_eq!(runs.len(), 1);
    let emissions = store.query_emissions_for_run("R1").unwrap();
    assert!(emissions.is_empty());
}

#[test]
fn store_write_run_and_emissions_transactional_on_sqlite() {
    let store = store_with(SqliteAdapter::open_in_memory().unwrap());
    let mut run = sample_run("R1", "T1");
    let emissions = vec![kf_memory_store::EmittedFileRecord {
        id: None,
        path: "src/foo.rs".into(),
        sha256: "abcdef0123456789".into(),
        bytes: 42,
        before_hash: None,
        existed: false,
        timestamp: None,
    }];
    store
        .write_run_and_emissions(&mut run, &emissions, 1)
        .unwrap();
    let emissions = store.query_emissions_for_run("R1").unwrap();
    assert_eq!(emissions.len(), 1);
    assert_eq!(
        emissions[0].properties["path"].as_str().unwrap(),
        "src/foo.rs"
    );
}

#[test]
fn store_write_emission_records_sequential_on_in_memory() {
    let store = store_with(InMemoryAdapter::new());
    let emissions = vec![kf_memory_store::EmittedFileRecord {
        id: None,
        path: "src/foo.rs".into(),
        sha256: "abcdef0123456789".into(),
        bytes: 42,
        before_hash: None,
        existed: false,
        timestamp: None,
    }];
    let ids = store
        .write_emission_records("R1", "T1", 1, &emissions)
        .unwrap();
    assert_eq!(ids.len(), 1);
    let q = MemoryQuery {
        kind: Some("emission".into()),
        ..Default::default()
    };
    assert_eq!(store.adapter().query(&q).unwrap().len(), 1);
}

#[test]
fn store_query_emissions_filters_by_task_id() {
    let store = store_with(InMemoryAdapter::new());
    let emissions = vec![kf_memory_store::EmittedFileRecord {
        id: None,
        path: "a".into(),
        sha256: "abcdef0123456789".into(),
        bytes: 1,
        before_hash: None,
        existed: false,
        timestamp: None,
    }];
    store
        .write_emission_records("R1", "T1", 0, &emissions)
        .unwrap();
    store
        .write_emission_records("R2", "T2", 0, &emissions)
        .unwrap();
    assert_eq!(store.query_emissions("T1").unwrap().len(), 1);
    assert_eq!(store.query_emissions("T2").unwrap().len(), 1);
}

#[test]
fn store_evict_overflow_reports_excess() {
    let adapter = InMemoryAdapter::new();
    for i in 0..5 {
        adapter
            .write(&obj(&format!("o{i}"), "obs", &format!("2024-01-0{i}")))
            .unwrap();
    }
    let store = MemoryStore::new(
        adapter,
        MemoryStoreOptions {
            ttl_ms: 0,
            max_entries: 3,
        },
    );
    assert_eq!(store.evict_overflow().unwrap(), 2);
}

#[test]
fn store_evict_expired_counts_old_entries() {
    let adapter = InMemoryAdapter::new();
    adapter
        .write(&obj("a", "obs", "2000-01-01T00:00:00Z"))
        .unwrap();
    let store = MemoryStore::new(
        adapter,
        MemoryStoreOptions {
            ttl_ms: 1,
            max_entries: 0,
        },
    );
    assert_eq!(store.evict_expired().unwrap(), 1);
}

#[test]
fn store_evict_disabled_when_zero() {
    let store = store_with(InMemoryAdapter::new());
    assert_eq!(store.evict_expired().unwrap(), 0);
    assert_eq!(store.evict_overflow().unwrap(), 0);
}

#[test]
fn store_evict_expired_actually_deletes() {
    let adapter = InMemoryAdapter::new();
    adapter
        .write(&obj("old", "obs", "2000-01-01T00:00:00Z"))
        .unwrap();
    adapter
        .write(&obj("new", "obs", "2099-01-01T00:00:00Z"))
        .unwrap();
    let store = MemoryStore::new(
        adapter,
        MemoryStoreOptions {
            ttl_ms: 1,
            max_entries: 0,
        },
    );
    assert_eq!(store.evict_expired().unwrap(), 1);
    assert_eq!(store.adapter().stats().unwrap().total_objects, 1);
    assert!(store.adapter().read("old").unwrap().is_none());
    assert!(store.adapter().read("new").unwrap().is_some());
}

#[test]
fn store_evict_overflow_actually_deletes_oldest() {
    let adapter = InMemoryAdapter::new();
    for i in 0..5 {
        adapter
            .write(&obj(&format!("o{i}"), "obs", &format!("2024-01-0{i}")))
            .unwrap();
    }
    let store = MemoryStore::new(
        adapter,
        MemoryStoreOptions {
            ttl_ms: 0,
            max_entries: 3,
        },
    );
    assert_eq!(store.evict_overflow().unwrap(), 2);
    assert_eq!(store.adapter().stats().unwrap().total_objects, 3);
    // query returns newest-first; o4/o3 kept, o0/o1 (oldest) evicted.
    assert!(store.adapter().read("o0").unwrap().is_none());
    assert!(store.adapter().read("o1").unwrap().is_none());
    assert!(store.adapter().read("o4").unwrap().is_some());
}

#[test]
fn store_evict_overflow_actually_deletes_on_sqlite() {
    let store = MemoryStore::new(
        SqliteAdapter::open_in_memory().unwrap(),
        MemoryStoreOptions {
            ttl_ms: 0,
            max_entries: 2,
        },
    );
    for i in 0..4 {
        store
            .adapter()
            .write(&obj(&format!("o{i}"), "obs", &format!("2024-01-0{i}")))
            .unwrap();
    }
    assert_eq!(store.evict_overflow().unwrap(), 2);
    assert_eq!(store.adapter().stats().unwrap().total_objects, 2);
    assert!(store.adapter().read("o0").unwrap().is_none());
    assert!(store.adapter().read("o1").unwrap().is_none());
    assert!(store.adapter().read("o3").unwrap().is_some());
}

#[test]
fn store_create_with_sqlite_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("mem.db");
    let store = MemoryStore::create(&db, MemoryStoreOptions::default()).unwrap();
    let run = sample_run("R1", "T1");
    store.write_run_record(&run).unwrap();
    assert_eq!(store.query_runs(None).unwrap().len(), 1);
}

#[test]
fn store_is_send_sync_via_arc() {
    // Compile-time assertion: the trait object behind Arc compiles, so
    // MemoryStore can move across threads (Send+Sync adapters).
    let store = MemoryStore::new(InMemoryAdapter::new(), MemoryStoreOptions::default());
    let _arc: Arc<MemoryStore> = Arc::new(store);
}

fn sample_run(run_id: &str, task_id: &str) -> kf_memory_store::RunRecord {
    kf_memory_store::RunRecord {
        run_id: run_id.into(),
        task_id: task_id.into(),
        description: "do the thing".into(),
        language: "python".into(),
        task_family: Some("general".into()),
        mode: "artifact".into(),
        model: "qwen".into(),
        provider_key: "".into(),
        provider_type: "".into(),
        base_url: None,
        outcome: "pass".into(),
        outcome_class: "pass".into(),
        routing_lesson: "reward".into(),
        final_verdict: "pass".into(),
        source_of_truth: "verifier".into(),
        final_action: "accept".into(),
        tokens: 100,
        duration_ms: 5000,
        turns: 3,
        validator_duration_ms: 0,
        verifier_overall: None,
        files_emitted: 0,
        total_bytes_emitted: 0,
        emissions: vec![],
        emission_ids: vec![],
        timestamp: "2024-01-01T00:00:00Z".into(),
    }
}
