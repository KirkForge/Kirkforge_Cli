//! Scenario: config isolation.
//!
//! Pins that the `KF_CODE_DATA_DIR` env var fully isolates the binary
//! from the user's real config.  A test that writes a custom model and
//! host to an isolated data dir must not see or affect the real config.
//! Regression: C-005 (config leaked across test runs).

use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[test]
fn isolated_env_does_not_leak_config() {
    if !shard::shard_gate("isolated_env_does_not_leak_config") {
        return;
    }

    // Create two isolated envs with different mock URLs.
    let env_a = IsolatedEnv::new("http://mock-a.example.com:11434", "model-a");
    let env_b = IsolatedEnv::new("http://mock-b.example.com:11434", "model-b");

    // Read the configs and confirm they are different.
    let config_a =
        std::fs::read_to_string(env_a.data_dir().join("config.toml")).expect("read config A");
    let config_b =
        std::fs::read_to_string(env_b.data_dir().join("config.toml")).expect("read config B");

    assert!(
        config_a.contains("mock-a"),
        "config A should contain mock-a: {config_a}"
    );
    assert!(
        config_b.contains("mock-b"),
        "config B should contain mock-b: {config_b}"
    );
    assert!(
        !config_a.contains("mock-b"),
        "config A should NOT contain mock-b"
    );
    assert!(
        !config_b.contains("mock-a"),
        "config B should NOT contain mock-a"
    );
}
