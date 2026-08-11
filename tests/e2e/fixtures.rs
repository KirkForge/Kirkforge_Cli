//! Fixture helpers: seed config.toml, hooks dirs, MCP server specs,
//! and `.kirk/` state into the isolated HOME before launch.

use std::path::Path;

/// Write a minimal `config.toml` into the data dir, pointing the binary
/// at the mock provider.
#[allow(dead_code)]
pub fn seed_config(data_dir: &Path, mock_url: &str, model: &str) {
    // The `[adapter_routing]` table forces the e2e model to the Ollama
    // adapter regardless of name-prefix guessing, so scenarios can assert
    // the `/api/chat` path deterministically. Without it, `e2e-test-model`
    // falls through `adapter_kind_for_default` to OpenAiCompat and hits
    // `/v1/chat/completions` instead.
    let config = format!(
        "default_model = \"{model}\"\n\
         ollama_host = \"{mock_url}\"\n\
         auto_approve = false\n\
         [adapter_routing]\n\
         \"e2e-\" = \"Ollama\"\n"
    );
    std::fs::write(data_dir.join("config.toml"), config).expect("seed config.toml");
}

/// Write a config that auto-approves all tool calls (for scenarios
/// that don't test the approval flow).
pub fn seed_config_auto_approve(data_dir: &Path, mock_url: &str, model: &str) {
    let config = format!(
        "default_model = \"{model}\"\n\
         ollama_host = \"{mock_url}\"\n\
         auto_approve = true\n\
         [adapter_routing]\n\
         \"e2e-\" = \"Ollama\"\n"
    );
    std::fs::write(data_dir.join("config.toml"), config).expect("seed auto-approve config.toml");
}

/// Ensure the sessions directory exists inside data_dir.
#[allow(dead_code)]
pub fn seed_sessions_dir(data_dir: &Path) {
    std::fs::create_dir_all(data_dir.join("sessions")).expect("seed sessions dir");
}

/// Create a `.kirk/` directory inside the project root (used as the
/// sandbox / working directory for the test).
#[allow(dead_code)]
pub fn seed_kirk_dir(project_dir: &Path) {
    std::fs::create_dir_all(project_dir.join(".kirk")).expect("seed .kirk dir");
}

/// Write a placeholder hooks directory.
#[allow(dead_code)]
pub fn seed_hooks_dir(data_dir: &Path) {
    std::fs::create_dir_all(data_dir.join("hooks")).expect("seed hooks dir");
}
