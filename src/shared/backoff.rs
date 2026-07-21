//! Shared exponential backoff helper.
//!
//! Used by both model-request retries and tool-call retries so the whole
//! CLI uses one deterministic backoff policy.

/// Compute the backoff for retry `attempt` (1-indexed).
///
/// Uses exponential backoff starting at 1 s with a small deterministic
/// jitter (up to 250 ms per attempt, capped at 1 s). The jitter is
/// computed from the attempt number rather than a random source so tests
/// are stable and no new dependency is required.
pub fn retry_backoff(attempt: u32) -> std::time::Duration {
    let shift = (attempt - 1).min(63);
    let base_s = 1u64 << shift;
    let jitter_ms = (attempt as u64).saturating_mul(250).min(1000);
    std::time::Duration::from_millis(base_s.saturating_mul(1000).saturating_add(jitter_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_with_capped_jitter() {
        let b1 = retry_backoff(1);
        let b2 = retry_backoff(2);
        let b3 = retry_backoff(3);

        // Base doubles each attempt; jitter is small (≤1 s).
        assert!(b1 >= std::time::Duration::from_secs(1));
        assert!(b1 <= std::time::Duration::from_millis(1250));

        assert!(b2 >= std::time::Duration::from_secs(2));
        assert!(b2 <= std::time::Duration::from_millis(2500));

        assert!(b3 >= std::time::Duration::from_secs(4));
        assert!(b3 <= std::time::Duration::from_millis(5000));

        assert!(b3 > b2 && b2 > b1);
    }
}
