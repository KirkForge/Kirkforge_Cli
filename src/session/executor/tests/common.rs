// Shared helpers for the executor test sub-modules. Split out from the
// former single-file `mod.rs` (WO 15.5). Pure refactor: the helpers are
// moved verbatim so every dispatch/turn/loop/approval test continues to
// route through the same `MockAdapter` and fixtures.

use super::super::*;
use crate::adapters::ModelAdapter;
use crate::shared::test_util::remove_test_file;
use crate::shared::{
    Config, FinishReason, Message, ModelInfo, StreamEvent, ToolCallStyle, ToolDef, ToolOutcome,
};
use crate::tools::{Tool, ToolContext};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// RAII guard that removes a temp file when dropped. Used by plan-mode
/// tests that need a real, readable file on disk.
pub(super) struct CleanupFile(pub(super) std::path::PathBuf);

impl Drop for CleanupFile {
    fn drop(&mut self) {
        remove_test_file(&self.0);
    }
}

pub(super) fn never_cancelled() -> &'static AtomicBool {
    static NC: std::sync::LazyLock<AtomicBool> =
        std::sync::LazyLock::new(|| AtomicBool::new(false));
    &NC
}

pub(super) fn cfg(exe: &Executor) -> std::sync::RwLockReadGuard<'_, Config> {
    crate::shared::read_shared_config(&exe.config)
}

pub(super) struct MockAdapter {
    pub(super) first_events: Vec<StreamEvent>,

    pub(super) followup_events: Vec<StreamEvent>,
    pub(super) info: ModelInfo,
    pub(super) call_count: Arc<Mutex<usize>>,
}

impl MockAdapter {
    pub(super) fn new(events: Vec<StreamEvent>, info: ModelInfo) -> Self {
        Self {
            first_events: events,
            followup_events: vec![
                StreamEvent::Text("Done.".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            ],
            info,
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    pub(super) fn with_followup_events(mut self, events: Vec<StreamEvent>) -> Self {
        self.followup_events = events;
        self
    }
}

#[async_trait::async_trait]
impl ModelAdapter for MockAdapter {
    fn model_info(&self) -> ModelInfo {
        self.info.clone()
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let mut count = self.call_count.lock().unwrap();
        let is_first = *count == 0;
        *count += 1;
        drop(count);

        let (tx, rx) = mpsc::channel(64);
        let events = if is_first {
            self.first_events.clone()
        } else {
            self.followup_events.clone()
        };
        tokio::spawn(async move {
            for ev in events {
                let _ = tx.send(ev).await;
            }
        });
        Ok(rx)
    }
}

#[derive(Clone)]
pub(super) struct MockTool {
    pub(super) def: ToolDef,
    pub(super) captured_args: Arc<Mutex<Option<serde_json::Value>>>,
    pub(super) outcome: ToolOutcome,
}

#[async_trait::async_trait]
impl Tool for MockTool {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        *self.captured_args.lock().unwrap() = Some(args);
        self.outcome.clone()
    }
}

pub(super) fn make_info() -> ModelInfo {
    ModelInfo {
        name: "test-model".into(),
        supports_thinking: false,
        tool_call_format: ToolCallStyle::Native,
        max_context_tokens: 8192,
        recommended_temperature: 0.7,
        supports_images: false,
        supports_cache: false,
    }
}

pub(super) fn make_config(auto_approve: bool) -> Config {
    let mut cfg = Config::default();
    cfg.model.default_model = "test".into();
    cfg.model.ollama_host = "https://gateway.example.com".into();
    cfg.security.auto_approve = auto_approve;
    cfg.security.bash_sandbox_workdir = false;
    cfg.session.carryover_enabled = false;
    cfg.session.preserve_recent_messages = 2;
    cfg.tools.max_tool_calls_per_turn = 10;
    cfg.tools.max_persona_turns = 10;
    cfg.model.request_timeout_secs = 300;
    cfg.session.checkpoint_interval_messages = 0;
    cfg.display.memory_enabled = false;
    cfg.display.memory_max_tokens = 0;
    cfg.display.memory_top_n = 0;
    cfg
}

pub(super) fn make_executor(
    adapter: Box<dyn ModelAdapter>,
    tools: Vec<Arc<dyn Tool>>,
    config: Config,
) -> anyhow::Result<Executor> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let temp_dir = std::env::temp_dir();
    let log_path = temp_dir.join(format!(
        "kf-code-test-{}-{}.ndjson",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    remove_test_file(&log_path);
    let (conversation, _outcome) = ConversationLog::open(log_path).unwrap();
    let mut composite = crate::session::toolset::CompositeToolset::empty();
    composite.add(Box::new(crate::session::toolset::VecToolset::new(
        "test", tools,
    )));
    Executor::with_log(adapter, composite, config, conversation, None)
}

#[cfg(unix)]
pub(super) fn temp_hooks_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    (tmp, hooks_dir)
}

/// Tool that sleeps for a fixed duration before returning. Used to exercise
/// cancellation mid-batch. An optional `start_tx` signals when `run` begins so
/// tests can set cancellation deterministically after the first call starts.
pub(super) struct SleepingTool {
    pub(super) def: ToolDef,
    pub(super) sleep_ms: u64,
    pub(super) call_count: Arc<Mutex<usize>>,
    pub(super) start_tx: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

#[async_trait::async_trait]
impl Tool for SleepingTool {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        if let Ok(mut guard) = self.start_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        *self.call_count.lock().unwrap() += 1;
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        ToolOutcome::Success {
            content: "done".into(),
        }
    }
}
