# Background Bash Jobs

**Source:** vix (`internal/daemon/tools.go` bash tool, `internal/daemon/session.go` BashJobRegistry)
**Goal:** Spawn long-running commands (builds, tests, installs) as background jobs. Continue the agent conversation while the job runs. Poll for completion when needed.

## Why

Today, `bash` tool blocks until the command finishes (with a timeout). For a 30-second build, the agent sits idle. With background jobs:
1. Agent spawns `cargo build` as a background job → gets job ID
2. Agent continues working (reading files, planning) while build runs
3. Agent checks job status when it needs the result
4. Job output is available even if the agent moves on

## API

```rust
pub struct BashJob {
    pub id: String,
    pub command: String,
    pub status: JobStatus,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub enum JobStatus {
    Running,
    Completed(i32),
    Failed(String),
    TimedOut,
}
```

Tool parameters for `bash`:
```json
{
    "command": "cargo build",
    "background": true,       // new — spawn and return immediately
    "timeout": 300
}
```

New tools:
- `bash_status` — check job status by ID
- `bash_output` — retrieve job stdout/stderr
- `bash_cancel` — kill a running job

## Tool Definitions

```rust
// bash: add "background" parameter (default false)
"background": {
    "type": "boolean",
    "description": "Run in background and return immediately (use bash_status to check)",
    "default": false
}

// bash_status: new tool
ToolDef {
    name: "bash_status",
    description: "Check the status of a background bash job",
    parameters: json!({
        "type": "object",
        "properties": {
            "job_id": { "type": "string", "description": "Job ID from bash --background" }
        },
        "required": ["job_id"]
    }),
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/tools/bash.rs` | Add background mode: spawn via `tokio::spawn`, return job ID immediately |
| `src/tools/mod.rs` | Register `bash_status` tool |
| `src/session/` | New `bash_jobs.rs` module — `BashJobRegistry` (HashMap of running jobs) |
| `src/session/executor.rs` | Job registry shared with executor, cleaned up on session end |