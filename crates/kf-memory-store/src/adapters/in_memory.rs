//! In-memory adapter. Port of `memory-palace/src/adapters/in-memory.ts`.

use std::sync::Mutex;

use anyhow::Result;

use crate::adapters::MemoryAdapter;
use crate::types::{MemoryObject, MemoryQuery, MemoryStats};

pub struct InMemoryAdapter {
    objects: Mutex<Vec<MemoryObject>>,
}

impl Default for InMemoryAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAdapter {
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(Vec::new()),
        }
    }

    pub fn clear(&self) {
        self.objects
            .lock()
            .expect("in-memory lock poisoned")
            .clear();
    }
}

impl MemoryAdapter for InMemoryAdapter {
    fn write(&self, obj: &MemoryObject) -> Result<()> {
        self.objects
            .lock()
            .expect("in-memory lock poisoned")
            .push(obj.clone());
        Ok(())
    }

    fn read(&self, id: &str) -> Result<Option<MemoryObject>> {
        let guard = self.objects.lock().expect("in-memory lock poisoned");
        Ok(guard.iter().find(|o| o.id == id).cloned())
    }

    fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryObject>> {
        let guard = self.objects.lock().expect("in-memory lock poisoned");
        let mut results: Vec<MemoryObject> = guard
            .iter()
            .filter(|o| q.kind.as_deref().is_none_or(|k| o.kind == k))
            .filter(|o| {
                q.tags
                    .as_deref()
                    .is_none_or(|tags| tags.iter().any(|t| o.tags.iter().any(|ot| ot == t)))
            })
            .filter(|o| q.since.as_deref().is_none_or(|s| o.timestamp.as_str() >= s))
            .cloned()
            .collect();
        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if let Some(limit) = q.limit {
            results.truncate(limit);
        }
        Ok(results)
    }

    fn stats(&self) -> Result<MemoryStats> {
        let guard = self.objects.lock().expect("in-memory lock poisoned");
        let last_write = guard
            .last()
            .map(|o| o.timestamp.clone())
            .unwrap_or_else(|| "never".to_string());
        Ok(MemoryStats {
            total_objects: guard.len(),
            last_write,
        })
    }
}
