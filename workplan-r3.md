# WO 23.7-R3: Move executor construction after semaphore acquire

## Root cause

WO 23.7-R3 was written against pre-R1/R2 code where `Task::run()` (background path)
inserted a `TaskHandle` into the manager (original line 194) **before** attempting
semaphore acquisition (original line 203). On reject, the handle leaked permanently
— `task_output` would report "still running" for a task that was never started.

The WO's reality check conflated "TaskHandle allocation" with "executor construction."
The actual `Executor::with_log_and_undo()` call was always inside `tokio::spawn`
(after the semaphore), but the TaskHandle was a leaked promise.

## Status: R3 already solved by R1+R2

R1+R2 restructured the background path in `src/tools/task.rs`:

```
Line 233-250: acquire semaphore (reject → try_acquire, queue → acquire_owned().await)
Line 251-257: insert TaskHandle ONLY after permit held
Line 259-269: tokio::spawn { spawner.run_task(request).await; drop(permit); ... }
```

Current ordering: semaphore → TaskHandle → tokio::spawn → executor. Correct.

On reject (line 237-241): early return before TaskHandle exists. No leak. No executor.

## Remaining work (XS — tests + docs only)

### Files to touch

1. **`src/tools/task.rs`** (tests section, after line 1013)
   - Add test: `task_reject_mode_does_not_leak_task_handle`
   - Verify `TaskManager::tasks` is empty after a rejected background task
   - Uses existing `BlockingSpawner` + `Task::with_config(manager, 1, Reject)`

2. **`docs/workorders/23.7-task-concurrency-semaphore.md`** (line 83)
   - Update R3 defer note: "R3 resolved by R1+R2 restructuring. Semaphore acquire
     moved before TaskHandle insertion. Executor construction was already inside
     tokio::spawn. Remaining: explicit leak-proof test."

### Exact changes

**test (task.rs, after line 1013):**
```rust
#[tokio::test]
async fn task_reject_mode_does_not_leak_task_handle() {
    let manager = Arc::new(Mutex::new(TaskManager::new()));
    let task = Task::with_config(
        Arc::clone(&manager),
        1,
        TaskConcurrencyMode::Reject,
    );
    let spawner: Arc<dyn TaskSpawner> = Arc::new(BlockingSpawner {
        started: Arc::new(tokio::sync::Notify::new()),
        finish: Arc::new(AtomicBool::new(false)),
    });
    let ctx = ToolContext::with_spawner(spawner.clone());
    // First task fills the semaphore
    let _ = task
        .run(&ctx, serde_json::json!({"prompt": "fill", "background": true}))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // Second task should be rejected — no handle leaked
    let outcome = task
        .run(&ctx, serde_json::json!({"prompt": "overflow", "background": true}))
        .await;
    assert!(
        matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
        "expected rejection, got {outcome:?}"
    );
    // Manager should have exactly 1 task, not 2
    let guard = manager.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(guard.tasks.len(), 1, "expected 1 handle, got {}", guard.tasks.len());
}
```

## Size estimate: XS (~15 lines of test + 2 lines of doc update)

## Risk assessment: NONE

The code change is already done. This is a test-only addition that validates the
existing R1+R2 ordering. No production code paths are modified. The test uses
existing test infrastructure (BlockingSpawner, Task::with_config, TaskManager).

## Gate

```
cargo test -p kf-code --lib tools::task::tests::task_reject_mode_does_not_leak_task_handle
cargo test --locked --workspace --no-fail-fast
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## What was skipped

- No production code refactoring needed — R1+R2 already fixed the ordering.
- No executor construction path changes — `InProcessTaskSpawner::run_task()` always
  ran inside `tokio::spawn` (after permit). The WO's reality check was about the
  TaskHandle, not the Executor.
- No memory profiling — the win is already realized: reject mode returns before
  any executor state is touched; queue mode blocks before any executor state.

---

# WO 23.8-R3: Configurable doom-loop remediation action

## Root cause
Doom-loop detection (`cost_tracking.rs:75-91`) currently has one behavior: emit a
`TurnEvent::DoomLoopDetected` and inject a hint message. There is no configurable
remediation — the user cannot choose between auto-switching to plan mode, halting,
or just getting a warning. The deferral note from WO 23.8 specifies three modes:
`auto_plan` (switch to plan mode), `halt` (stop the session), `warn_only` (current
behavior).

## Size: S
One new `String` field on `ToolConfig`, one 3-variant enum with `FromStr`, ~15 lines
of wiring in `cost_tracking.rs`, ~5 lines of env override, ~30 lines of tests. No
new dependencies. No architectural changes.

## Files to touch (7)

### 1. `src/shared/config/tools.rs` — add `doom_loop_action` field

**Line ~119** (after `budget_approaching_ratio` field, before struct closing brace at 120):
```rust
#[serde(default = "default_doom_loop_action")]
pub doom_loop_action: String,
```

**Line ~66** (near other default fns):
```rust
fn default_doom_loop_action() -> String {
    "auto_plan".to_string()
}
```

**Line ~157** (in `Default` impl, after `budget_approaching_ratio`):
```rust
doom_loop_action: default_doom_loop_action(),
```

**Line ~173** (in `tool_config_defaults_match_spec` test):
```rust
assert_eq!(cfg.doom_loop_action, "auto_plan");
```

### 2. `src/session/executor/cost_tracking.rs` — wire remediation action

This is the core change. Currently `observe_tool_outcome` (line 48-94) emits the
event and returns a hint. The remediation action must be decided here and surfaced
to the caller.

**Approach**: Change the return type from `Option<String>` to
`Option<DoomLoopOutcome>` where `DoomLoopOutcome` carries both the hint and the
requested action:

```rust
/// What to do when a doom loop is detected.
pub(crate) struct DoomLoopOutcome {
    /// Hint message to inject into the conversation.
    pub hint: String,
    /// Requested remediation action.
    pub action: DoomLoopAction,
}
```

The enum lives in `cost_tracking.rs` (it's only used by the executor internals):

```rust
/// Configurable doom-loop remediation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoomLoopAction {
    /// Switch to plan mode (read-only tools only).
    AutoPlan,
    /// Halt the session immediately.
    Halt,
    /// Emit the warning event only (current behavior).
    WarnOnly,
}

impl std::str::FromStr for DoomLoopAction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto_plan" => Ok(Self::AutoPlan),
            "halt" => Ok(Self::Halt),
            "warn_only" => Ok(Self::WarnOnly),
            _ => Err(format!(
                "unknown doom_loop_action '{s}': expected 'auto_plan', 'halt', or 'warn_only'"
            )),
        }
    }
}
```

**Line 48** — change signature:
```rust
pub(crate) fn observe_tool_outcome(
    &mut self,
    tool: &str,
    outcome: &ToolOutcome,
    event_tx: &mpsc::Sender<TurnEvent>,
    doom_action: DoomLoopAction,  // NEW PARAM
) -> Option<DoomLoopOutcome> {
```

**Lines 75-92** — when `self.doom_loop_tracker.observe(...)` returns `Some(hit)`:
- Always emit `MetricEvent::DoomLoop` and `TurnEvent::DoomLoopDetected` (unchanged).
- Return `Some(DoomLoopOutcome { hint: ..., action: doom_action })`.

**Lines 88-91** stay the same (event emission). The hint format on line 88-91 is
unchanged. The only difference is we now wrap the return in `DoomLoopOutcome` and
pass through the `doom_action`.

### 3. `src/session/executor/mod.rs` — update `observe_tool_outcome` wrapper

**Lines 739-746** — update the public method to accept and forward `DoomLoopAction`:
```rust
pub fn observe_tool_outcome(
    &mut self,
    tool: &str,
    outcome: &crate::shared::ToolOutcome,
    event_tx: &mpsc::Sender<TurnEvent>,
) -> Option<cost_tracking::DoomLoopOutcome> {
    let cfg = crate::shared::read_shared_config(&self.config);
    let action = cfg.tools.doom_loop_action.parse::<cost_tracking::DoomLoopAction>()
        .unwrap_or(cost_tracking::DoomLoopAction::AutoPlan);
    self.cost.observe_tool_outcome(tool, outcome, event_tx, action)
}
```

Re-export `DoomLoopAction` and `DoomLoopOutcome` from `mod.rs` so callers can use them.

### 4. `src/session/executor/turn.rs` — handle remediation actions

**Lines 983-991** and **1068-1076** — the two call sites of `observe_tool_outcome`.

Currently:
```rust
if let Some(hint) = self.observe_tool_outcome(&tc.name, &outcome_for_emit, event_tx) {
    self.conversation.append_async(Message { role: Role::User, content: hint, .. }).await?;
}
```

Change to:
```rust
if let Some(outcome) = self.observe_tool_outcome(&tc.name, &outcome_for_emit, event_tx) {
    self.conversation.append_async(Message {
        role: Role::User,
        content: outcome.hint,
        ..Default::default()
    }).await?;
    match outcome.action {
        cost_tracking::DoomLoopAction::AutoPlan => {
            self.set_plan_mode(true);
            // Inject system message so the model knows it's in plan mode.
            self.conversation.append_async(Message {
                role: Role::System,
                content: "[System: doom loop detected — switched to plan mode. Read-only tools only.]".into(),
                ..Default::default()
            }).await?;
        }
        cost_tracking::DoomLoopAction::Halt => {
            return Err(anyhow::anyhow!(
                "doom loop halted: {} failed {} times with: {}",
                tc.name, outcome.hint, /* need count from outcome */
            ));
        }
        cost_tracking::DoomLoopAction::WarnOnly => {
            // No action needed — the event and hint are already emitted/injected.
        }
    }
}
```

**Correction**: `DoomLoopOutcome` should also carry `count` and `tool` so the halt
message can be useful. Add fields:
```rust
pub(crate) struct DoomLoopOutcome {
    pub hint: String,
    pub action: DoomLoopAction,
    pub count: usize,
    pub tool: String,
}
```

Then halt becomes:
```rust
cost_tracking::DoomLoopAction::Halt => {
    return Err(anyhow::anyhow!(
        "doom loop halted: '{}' failed {} times",
        outcome.tool, outcome.count
    ));
}
```

### 5. `src/session/config/env_overrides.rs` — env override

**Line ~339** (after `KF_CODE_TOOL_TIMEOUT_SECS` block, before `KF_CODE_AUDIT_LOG_PATH`):
```rust
// KF_CODE_DOOM_LOOP_ACTION
if let Ok(val) = std::env::var("KF_CODE_DOOM_LOOP_ACTION") {
    if !val.is_empty() {
        cfg.tools.doom_loop_action = val;
    }
}
```

### 6. `src/session/config/mod.rs` — TOML merge + drift guard

**Line ~413** (after `tool_timeout_secs` merge, before `audit_log_path`):
```rust
if let Some(Value::String(v)) = table.get("doom_loop_action") {
    cfg.tools.doom_loop_action = v.clone();
}
```

**Drift guard updates** (all in `config_field_count_drift_guard`):
- `src/shared/config/mod.rs` line 25: comment `ToolConfig 25` → `ToolConfig 26`
- `src/shared/config/mod.rs` line 30: `CONFIG_FIELD_COUNT = 85` → `86`
- `src/session/config/mod.rs` line 1914: comment `ToolConfig=25` → `ToolConfig=26`
- `src/session/config/mod.rs` line 1917: assert literal `85` → `86`
- `src/session/config/mod.rs` line 1963: add `doom_loop_action = "auto_plan"` to drift TOML
- `src/session/config/mod.rs` line 2013: `MERGE_TOML_EXPECTED = 73` → `74`
- `src/session/config/mod.rs` line 2022: `ENV_OVERRIDE_EXPECTED = 69` → `70`

### 7. Tests

**`src/session/executor/cost_tracking.rs`** — unit tests for `DoomLoopAction::FromStr`:
```rust
#[test]
fn doom_loop_action_from_str_valid() {
    assert_eq!("auto_plan".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::AutoPlan);
    assert_eq!("halt".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::Halt);
    assert_eq!("warn_only".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::WarnOnly);
}

#[test]
fn doom_loop_action_from_str_invalid() {
    assert!("banish".parse::<DoomLoopAction>().is_err());
}
```

**`src/session/executor/tests/loop_.rs`** — integration test for each mode.

Extend `observe_tool_outcome_doom_after_threshold` (line 674) into three tests:
- `doom_outcome_warn_only_returns_hint` — with `DoomLoopAction::WarnOnly`, verify
  `observe_tool_outcome` returns `Some(DoomLoopOutcome)` with `action: WarnOnly`.
- `doom_outcome_auto_plan_returns_action` — with `DoomLoopAction::AutoPlan`, verify
  the returned outcome has `action: AutoPlan`.
- `doom_outcome_halt_returns_action` — with `DoomLoopAction::Halt`, verify
  `action: Halt`.

Note: the test `make_config` helper in `common.rs:129` does not need changing —
the default `doom_loop_action: "auto_plan"` will be set by `ToolConfig::default()`.

**`src/shared/config/tools.rs`** — TOML round-trip test (existing
`tool_config_toml_overrides_defaults` at line 180):
```rust
doom_loop_action = "halt"
```
Add assertion: `assert_eq!(cfg.doom_loop_action, "halt");`

## Gate command
```bash
cargo test --locked --workspace --no-fail-fast
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Summary of exact changes

| File | Line(s) | Change |
|------|---------|--------|
| `src/shared/config/tools.rs` | ~66 | Add `fn default_doom_loop_action()` |
| `src/shared/config/tools.rs` | ~119 | Add `doom_loop_action: String` field |
| `src/shared/config/tools.rs` | ~157 | Add `doom_loop_action` in Default impl |
| `src/shared/config/tools.rs` | ~180 | Add TOML override test assertion |
| `src/shared/config/tools.rs` | ~173 | Add default assertion in spec test |
| `src/shared/config/mod.rs` | 25, 30 | Bump comments + CONFIG_FIELD_COUNT 85→86 |
| `src/session/config/mod.rs` | ~413 | Add TOML merge for `doom_loop_action` |
| `src/session/config/mod.rs` | ~1914, 1917 | Drift guard count bumps |
| `src/session/config/mod.rs` | ~1963 | Add to drift TOML table |
| `src/session/config/mod.rs` | 2013, 2022 | MERGE_TOML_EXPECTED, ENV_OVERRIDE_EXPECTED +1 |
| `src/session/config/env_overrides.rs` | ~339 | Add `KF_CODE_DOOM_LOOP_ACTION` |
| `src/session/executor/cost_tracking.rs` | top | Add `DoomLoopAction` enum + `FromStr` |
| `src/session/executor/cost_tracking.rs` | top | Add `DoomLoopOutcome` struct |
| `src/session/executor/cost_tracking.rs` | 48 | Change `observe_tool_outcome` signature (add `doom_action` param) |
| `src/session/executor/cost_tracking.rs` | 88 | Wrap return in `DoomLoopOutcome` |
| `src/session/executor/cost_tracking.rs` | bottom | Add `FromStr` unit tests |
| `src/session/executor/mod.rs` | 37 | Re-export `DoomLoopAction`, `DoomLoopOutcome` |
| `src/session/executor/mod.rs` | 739-746 | Parse config + forward to `cost.observe_tool_outcome` |
| `src/session/executor/turn.rs` | 983-991 | Handle `AutoPlan`/`Halt`/`WarnOnly` after doom detection |
| `src/session/executor/turn.rs` | 1068-1076 | Same handling for second call site |
| `src/session/executor/tests/loop_.rs` | ~700+ | Add 3 tests for each action mode |

## Risks / notes

- **Two call sites in turn.rs** (lines 983 and 1068) — both must be updated identically.
  Miss one and half the code paths have no remediation.
- **Default `auto_plan` is a behavior change for existing users** who implicitly had
  `warn_only`. The deferral note says default to `auto_plan`, which switches to plan
  mode on doom loop. This is arguably better than just warning, but it is a change.
  Document in CHANGELOG.
- **`Halt` returns `Err` from `run_turn`** — this propagates up to the `run` loop in
  `loop_.rs:496` which already handles `Err` by flushing carryover and exiting (line
  502-504). No additional handling needed.
- **`AutoPlan` calls `self.set_plan_mode(true)`** — this sets the flag but does NOT
  emit a `TurnEvent` about plan-mode activation (the TUI plan-mode toggle does via
  `plan_rx` in `loop_.rs:245`). We inject a system message instead, which is visible
  in the chat. The TUI plan-mode indicator won't light up unless we also emit a
  mechanism for it. **Acceptable for S scope** — the model gets the system message and
  the dispatch layer enforces plan mode (tool denial). The TUI indicator is a
  follow-up.
