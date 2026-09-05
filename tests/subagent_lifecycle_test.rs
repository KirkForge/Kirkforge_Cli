//! WO 35.5 chain 1 — subagent lifecycle integration tests.
//!
//! agent → subagent → permission → workspace → cancellation, against the
//! scripted mock provider (see `common`). The real `InProcessTaskSpawner`
//! boots a nested executor whose model traffic hits the wiremock server;
//! every assertion crosses at least one component seam.
//!
//! Unix-gated like the `WorktreeSession` tests (git worktrees + fork).

#![cfg(unix)]

mod common;

use common::{MockOllama, Reply};
use kf_code::session::executor::{ApprovalRequest, ApprovalResponse};
use kf_code::session::task_spawner::InProcessTaskSpawner;
use kf_code::session::worktree::WorktreeSession;
use kf_code::shared::{Config, SharedConfig};
use kf_code::tools::task::{
    Task, TaskConcurrencyMode, TaskManager, TaskRequest, TaskSpawner, TaskStatus,
};
use kf_code::tools::{Tool, ToolContext};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// The spawner's patch marker (WO 35.2). `pub(crate)` in the crate, so the
// literal is pinned here; if the marker changes, this test fails — that is
// the point.
const PATCH_MARKER: &str =
    "--- subagent patch (uncommitted worktree changes; apply in the parent with `git apply`) ---";

const MODEL: &str = "e2e-35-5-model";

// Serialize tests that scan the shared `kf-code-task-<pid>-*` temp
// namespace (mirrors the spawner's unit tests).
static TMP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn task_temp_dirs() -> Vec<PathBuf> {
    let prefix = format!("kf-code-task-{}-", std::process::id());
    let mut dirs: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn session_worktrees() -> Vec<PathBuf> {
    let prefix = format!("kf-code-session-task-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("git spawn failed: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// Parent sandbox fixture: a small git repo with one commit. The subagent
// worktree branches from here (sandbox_dir is a git repo → used as root).
fn init_parent_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path();
    git(repo, &["init"]);
    git(repo, &["config", "user.email", "test@test"]);
    git(repo, &["config", "user.name", "test"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "init"]);
    tmp
}

fn subagent_config(
    parent_repo: &std::path::Path,
    audit_path: &std::path::Path,
    auto_approve: bool,
    worktree_enabled: bool,
) -> SharedConfig {
    let mut cfg = Config::default();
    cfg.session.artifact_policy = if worktree_enabled {
        kf_code::shared::ArtifactPolicy::PatchOnly
    } else {
        kf_code::shared::ArtifactPolicy::DirectWrite
    };
    cfg.security.sandbox_dir = Some(parent_repo.to_string_lossy().to_string());
    cfg.security.auto_approve = auto_approve;
    cfg.security.audit_log_path = Some(audit_path.to_path_buf());
    cfg.security.bash_sandbox_workdir = false;
    cfg.model
        .adapter_routing
        .insert("e2e-".to_string(), "Ollama".to_string());
    cfg.model.request_timeout_secs = 30;
    Arc::new(std::sync::RwLock::new(cfg))
}

// Approver that records every forwarded request (tool name + args) and
// answers all of them with `answer`.
fn spawn_parent_approver(
    spawner: &InProcessTaskSpawner,
    answer: ApprovalResponse,
) -> Arc<Mutex<Vec<(String, serde_json::Value)>>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
    spawner.set_parent_approval(tx);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            seen_clone
                .lock()
                .unwrap()
                .push((req.tool_name.clone(), req.args.clone()));
            let _ = req.response.send(answer.clone());
        }
    });
    seen
}

// (a) Approve path: the coder's write lands in its OWN worktree (not the
// parent repo), and the returned summary carries the WO 35.2 patch marker
// with a diff that applies cleanly to a fresh checkout.
#[tokio::test]
async fn coder_subagent_writes_own_worktree_and_returns_applicable_patch() {
    let _lock = TMP_LOCK.lock().await;
    let audit = tempfile::NamedTempFile::new().unwrap();
    let parent = init_parent_repo();
    let cfg = subagent_config(parent.path(), audit.path(), false, true);

    let before_tasks = task_temp_dirs();
    let before_worktrees = session_worktrees();
    let mock = MockOllama::start(
        vec![
            Reply::tool(
                "write_file",
                serde_json::json!({
                    "path": "{WORKTREE}/coder_note.txt",
                    "content": "written by coder subagent\n",
                }),
            ),
            Reply::text("DONE").with_usage(3, 5),
        ],
        before_worktrees,
    )
    .await;

    let spawner = InProcessTaskSpawner::new(cfg, MODEL.to_string(), mock.uri(), None, false);
    let approvals = spawn_parent_approver(&spawner, ApprovalResponse::Approved);

    let result = spawner
        .run_task(TaskRequest {
            prompt: "write the note".to_string(),
            persona: "coder".to_string(),
            model: None,
            max_turns: 1,
            cancel: None,
            owner: None,
            subagent_depth: 1,
            pending_messages: None,
        })
        .await
        .expect("run_task should succeed");

    // The destructive write was forwarded to the parent approval channel.
    {
        let seen = approvals.lock().unwrap();
        assert_eq!(seen.len(), 1, "one approval request expected: {seen:?}");
        assert_eq!(seen[0].0, "write_file");
    }

    // Writes landed in the subagent's own worktree, not the parent repo.
    // (The worktree itself is already gone by now — run_task's
    // WorktreeSession drops before returning — so compare paths, and
    // assert its absence below.)
    let worktree = mock
        .found_worktree()
        .expect("mock substituted the worktree path");
    assert!(
        worktree != parent.path(),
        "subagent must not write in the parent repo"
    );
    assert!(
        !parent.path().join("coder_note.txt").exists(),
        "parent repo must stay untouched"
    );

    // The summary carries the WO 35.2 patch marker + an appliable diff.
    let (_, patch) = result.split_once(PATCH_MARKER).unwrap_or_else(|| {
        let tool_results: Vec<String> = mock
            .request_bodies()
            .iter()
            .flat_map(|b| b["messages"].as_array().cloned().unwrap_or_default())
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .collect();
        panic!("summary must carry the patch marker, got: {result}; tool results the model saw: {tool_results:?}")
    });
    assert!(
        patch.contains("coder_note.txt") && patch.contains("written by coder subagent"),
        "patch must contain the coder's edit: {patch}"
    );

    let clean = WorktreeSession::create(
        &format!("wo355-apply-{}", std::process::id()),
        parent.path(),
    )
    .await
    .expect("clean worktree");
    let patch_file = parent.path().join("wo355.patch");
    // trim_start only: the diff's trailing newline is part of the format
    // and `git apply` rejects a hunk whose last line lost it.
    std::fs::write(&patch_file, patch.trim_start()).unwrap();
    git(
        clean.path(),
        &["apply", patch_file.to_string_lossy().as_ref()],
    );
    assert_eq!(
        std::fs::read_to_string(clean.path().join("coder_note.txt")).unwrap(),
        "written by coder subagent\n"
    );
    drop(clean);

    // Both worktree (spawner drop) and task temp dir are cleaned up.
    assert!(!worktree.exists(), "subagent worktree must be removed");
    let leftover = task_temp_dirs();
    let leaked: Vec<_> = leftover
        .into_iter()
        .filter(|p| !before_tasks.contains(p))
        .collect();
    assert!(leaked.is_empty(), "kf-code-task-* leak: {leaked:?}");
}

// (b) Deny path: the parent's denial is the tool result the subagent model
// sees, nothing is written, and no patch is returned.
#[tokio::test]
async fn subagent_destructive_write_denied_via_parent_approval() {
    let _lock = TMP_LOCK.lock().await;
    let audit = tempfile::NamedTempFile::new().unwrap();
    let parent = init_parent_repo();
    let cfg = subagent_config(parent.path(), audit.path(), false, true);

    let before_tasks = task_temp_dirs();
    let mock = MockOllama::start(
        vec![
            Reply::tool(
                "write_file",
                serde_json::json!({
                    "path": "{WORKTREE}/evil.txt",
                    "content": "should never land\n",
                }),
            ),
            Reply::text("DENIED-COMPLETE"),
        ],
        session_worktrees(),
    )
    .await;

    let spawner = InProcessTaskSpawner::new(cfg, MODEL.to_string(), mock.uri(), None, false);
    let approvals = spawn_parent_approver(
        &spawner,
        ApprovalResponse::DeniedWithReason("denied by test parent".to_string()),
    );

    let result = spawner
        .run_task(TaskRequest {
            prompt: "try to write".to_string(),
            persona: "coder".to_string(),
            model: None,
            max_turns: 1,
            cancel: None,
            owner: None,
            subagent_depth: 1,
            pending_messages: None,
        })
        .await
        .expect("run_task should succeed");

    {
        let seen = approvals.lock().unwrap();
        assert_eq!(seen.len(), 1, "denial request must reach the parent");
        assert_eq!(seen[0].0, "write_file");
    }

    // The denial flowed back as the tool result the model sees: the second
    // model request carries it in the conversation.
    let bodies = mock.request_bodies();
    assert_eq!(bodies.len(), 2, "expected follow-up model request");
    let second = serde_json::to_string(&bodies[1]).unwrap();
    assert!(
        second.contains("denied by test parent"),
        "model must see the parent's denial, got: {second}"
    );

    // Nothing was written: no patch marker, no parent-side file.
    assert!(
        !result.contains(PATCH_MARKER),
        "denied write must not produce a patch: {result}"
    );
    assert!(!parent.path().join("evil.txt").exists());
    if let Some(wt) = mock.found_worktree() {
        assert!(!wt.exists(), "subagent worktree must be cleaned up");
    }
    let leftover = task_temp_dirs();
    let leaked: Vec<_> = leftover
        .into_iter()
        .filter(|p| !before_tasks.contains(p))
        .collect();
    assert!(leaked.is_empty(), "kf-code-task-* leak: {leaked:?}");
}

// (c) Cancel path: the real `task` tool (background) + TaskManager::cancel
// mid-flight. The cancel token must kill the in-flight bash child — the
// task reaches Cancelled in seconds, not after the 8s sleep.
#[tokio::test]
async fn taskmanager_cancel_midflight_bash_exits_cooperatively() {
    let _lock = TMP_LOCK.lock().await;
    let audit = tempfile::NamedTempFile::new().unwrap();
    let mut cfg = Config::default();
    cfg.security.auto_approve = true;
    cfg.security.audit_log_path = Some(audit.path().to_path_buf());
    cfg.security.bash_sandbox_workdir = false;
    cfg.model
        .adapter_routing
        .insert("e2e-".to_string(), "Ollama".to_string());
    cfg.model.request_timeout_secs = 30;
    let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));

    let mock = MockOllama::start(
        vec![Reply::tool(
            "bash",
            serde_json::json!({"command": "sleep 8", "timeout": 20}),
        )],
        session_worktrees(),
    )
    .await;

    let manager = Arc::new(Mutex::new(TaskManager::new()));
    let task_tool = Task::with_config(manager.clone(), 4, TaskConcurrencyMode::Queue, 32, 3);
    let spawner = Arc::new(InProcessTaskSpawner::new(
        config,
        MODEL.to_string(),
        mock.uri(),
        None,
        false,
    ));
    let mut ctx = ToolContext::new();
    ctx.task_spawner = Some(spawner);

    let before_tasks = task_temp_dirs();
    let outcome = task_tool
        .run(
            &ctx,
            serde_json::json!({"prompt": "sleep for a long time", "background": true}),
        )
        .await;
    let content = match outcome {
        kf_code::shared::ToolOutcome::Success { content } => content,
        other => panic!("expected Success, got {other:?}"),
    };
    let task_id = content
        .split_whitespace()
        .find(|w| w.starts_with("task-"))
        .expect("task id in output")
        .trim_end_matches('.')
        .to_string();

    // Wait until the model call happened (bash is about to be in flight).
    let deadline = Instant::now() + Duration::from_secs(10);
    while mock.request_bodies().is_empty() {
        assert!(Instant::now() < deadline, "model never called the mock");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Poll until the task is Running (bash child spawned), then cancel.
    // The old 400ms sleep assumed bash started within that window — under
    // CI load it doesn't, and cancel hits a task with no in-flight bash to
    // kill, so the notified() never fires before the timeout.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = manager.lock().unwrap().status(&task_id);
        if matches!(status, Some(kf_code::tools::task::TaskStatus::Running)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task never reached Running: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let completed = manager
        .lock()
        .unwrap()
        .get(&task_id)
        .expect("task registered")
        .completed
        .clone();
    let notified = completed.notified();
    assert!(
        manager.lock().unwrap().cancel(&task_id),
        "cancel must be accepted while running"
    );

    tokio::time::timeout(Duration::from_secs(15), notified)
        .await
        .expect("task must finish before the 8s sleep runs out (15s hang guard)");
    {
        let guard = manager.lock().unwrap();
        let handle = guard.get(&task_id).unwrap();
        assert_eq!(handle.status(), TaskStatus::Cancelled);
        assert!(
            handle.cancelled_result.is_some(),
            "cancelled task retains its partial output"
        );
    }
    let leftover = task_temp_dirs();
    let leaked: Vec<_> = leftover
        .into_iter()
        .filter(|p| !before_tasks.contains(p))
        .collect();
    assert!(leaked.is_empty(), "kf-code-task-* leak: {leaked:?}");
}
