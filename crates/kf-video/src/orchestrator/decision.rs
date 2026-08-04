//! Append-only decision log. Each line is a `Decision` JSON record.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub category: String,
    pub choice: String,
    pub reason: String,
    #[serde(default)]
    pub options_considered: Vec<String>,
    #[serde(default)]
    pub rejected_because: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DecisionLog {
    path: Option<std::path::PathBuf>,
}

impl DecisionLog {
    pub fn open(project_dir: &Path) -> Self {
        let dir = project_dir.join("artifacts");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            path: Some(dir.join("decision_log.jsonl")),
        }
    }

    pub fn append(&self, d: &Decision) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        // ponytail: write the raw object so we can add metadata keys
        // (like `ts`) without changing the Decision struct.
        let mut obj = serde_json::to_value(d)?;
        if let Some(map) = obj.as_object_mut() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            map.insert("ts".into(), serde_json::Value::Number(ts.into()));
        }
        writeln!(f, "{}", serde_json::to_string(&obj)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_writes_ts_field() {
        let dir = tempfile::tempdir().unwrap();
        let log = DecisionLog::open(dir.path());
        log.append(&Decision {
            category: "test".into(),
            choice: "ok".into(),
            reason: "because".into(),
            options_considered: vec![],
            rejected_because: vec![],
        })
        .unwrap();
        let raw = std::fs::read_to_string(dir.path().join("artifacts/decision_log.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(v["category"], "test");
        let ts = v["ts"].as_u64().expect("ts missing");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            ts <= now && ts >= now.saturating_sub(5),
            "ts {ts} not within last 5s of now {now}"
        );
    }
}
