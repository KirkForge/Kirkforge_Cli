//! Shared test helpers for plugin3-core.
//!
//! This module is compiled into every build so that downstream crate
//! tests (e.g. `plugin3-cli`) can reuse the helpers. It is intended
//! for test code only; production code should not rely on it. It
//! centralises patterns that previously had to be copy-pasted into
//! every test module, most notably the `EnvGuard` used to temporarily
//! override `PLUGIN3_*_DIR` environment variables.

use std::ffi::OsStr;
use std::sync::{Mutex, MutexGuard};
use std::thread::ThreadId;

// ponytail: process-global reentrant mutex that serialises every
// env-var mutation made by plugin3-core tests. `std::env::set_var`
// is process-global; parallel `#[test]` runs read each other's
// overrides and produce flaky failures (plugin3-gaps.md § B8). A
// plain `Mutex` deadlocks when a single test nests `EnvGuard`s (e.g.
// `env_guard_restores_prior_value_some_branch`), so we count
// recursion depth per-thread and only release the underlying mutex
// when the outermost guard drops.

static ENV_TEST_MUTEX: ReentrantMutex = ReentrantMutex::new();

struct ReentrantState {
    holder: Option<ThreadId>,
    depth: usize,
}

struct ReentrantMutex {
    state: Mutex<ReentrantState>,
    mutex: Mutex<()>,
}

impl ReentrantMutex {
    const fn new() -> Self {
        Self {
            state: Mutex::new(ReentrantState {
                holder: None,
                depth: 0,
            }),
            mutex: Mutex::new(()),
        }
    }

    fn lock(&self) -> ReentrantMutexGuard<'_> {
        let current = std::thread::current().id();
        // ponytail: a test may panic while holding the env guard (the
        // `env_guard_restores_prior_value_on_panic` regression test does
        // exactly this inside `catch_unwind`). The guard's Drop restores
        // the env var during unwind, so the mutex invariant is actually
        // fine even though std marks it "poisoned". Recover with
        // `into_inner()` rather than letting PoisonError fail every
        // subsequent test.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.holder == Some(current) {
            // Same thread already holds the lock; just recurse.
            state.depth += 1;
            return ReentrantMutexGuard {
                mutex: self,
                inner: None,
            };
        }
        // Another thread (or none) holds it. Block on the real mutex.
        drop(state);
        let guard = self
            .mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.holder = Some(current);
        state.depth = 1;
        ReentrantMutexGuard {
            mutex: self,
            inner: Some(guard),
        }
    }
}

struct ReentrantMutexGuard<'a> {
    mutex: &'a ReentrantMutex,
    inner: Option<MutexGuard<'a, ()>>,
}

impl Drop for ReentrantMutexGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .mutex
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.depth > 1 {
            state.depth -= 1;
            return;
        }
        state.holder = None;
        state.depth = 0;
        // Release the underlying mutex only on the outermost drop.
        drop(state);
        drop(self.inner.take());
    }
}

/// Temporarily override an environment variable, restoring the prior
/// value (or unset state) when dropped.
///
/// The guard serialises with every other `EnvGuard` in the process,
/// including nested guards in the same thread, so parallel tests that
/// touch `PLUGIN3_*_DIR` do not race. All mutation of the process env
/// happens while the guard's lock is held; the prior value is restored
/// before the lock is released, so a panicking or failing test cannot
/// leak its override into a neighbour.
pub struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
    #[allow(dead_code)]
    guard: ReentrantMutexGuard<'static>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let guard = ENV_TEST_MUTEX.lock();
        let prior = std::env::var(key).ok();
        // SAFETY: the process-global env mutex is held, and the caller
        // is running inside a test that bails when the var is already
        // set by the developer's shell.
        unsafe { std::env::set_var(key, value) };
        Self { key, prior, guard }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Restore the env var while the lock is still held; the lock is
        // released automatically when `_lock` drops after this method.
        match &self.prior {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
