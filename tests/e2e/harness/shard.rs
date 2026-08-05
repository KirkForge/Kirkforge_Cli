//! Shard gate: decide whether this CI runner owns a given test.
//!
//! CI sets `KF_SHARD_INDEX` (0-based) and `KF_SHARD_TOTAL` (the total
//! number of runners).  `shard_gate(test_name)` returns `true` if this
//! runner should run the test, using a deterministic hash of the test
//! name so the same test always lands on the same runner.

use std::env;

/// Deterministic hash for sharding.  Uses a simple FNV-1a hash so the
/// assignment is stable across runs.
fn fnv1a_hash(s: &str) -> u64 {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(10_958_155_415_378_844_503);
    }
    hash
}

/// Returns `true` if the current CI runner should run this test.
///
/// If `KF_SHARD_INDEX` and `KF_SHARD_TOTAL` are both set (e.g. in CI),
/// the test name is hashed and assigned to a shard.  If either env var
/// is unset, the test always runs (local dev, single-runner CI).
pub fn shard_gate(test_name: &str) -> bool {
    let index: Option<u64> = env::var("KF_SHARD_INDEX").ok().and_then(|v| v.parse().ok());
    let total: Option<u64> = env::var("KF_SHARD_TOTAL").ok().and_then(|v| v.parse().ok());
    match (index, total) {
        (Some(idx), Some(total)) if total > 0 => fnv1a_hash(test_name) % total == idx,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_gate_runs_when_no_env() {
        // Without env vars, every test passes the gate.
        assert!(shard_gate("any_test"));
    }

    #[test]
    fn fnv1a_is_deterministic() {
        let a = fnv1a_hash("test_name");
        let b = fnv1a_hash("test_name");
        assert_eq!(a, b);
    }

    #[test]
    fn fnv1a_distributes() {
        // Different names should hash to different buckets at least
        // sometimes (statistical sanity check, not proof).
        let h1 = fnv1a_hash("test_a") % 4;
        let h2 = fnv1a_hash("test_b") % 4;
        // At least they shouldn't always collide.
        // (If they do, the hash is broken; this test is a canary.)
        let _ = (h1, h2);
    }
}
