# ADR-036: Docker Execution Mode

**Status:** Accepted (2026-07-21)

## Context

ChatGPT's cross-review named "Docker execution, ephemeral workspaces, Git worktrees, resource limits" as the sandboxing bar. The sandbox was path-based only — no process isolation. For "safe to run on a stranger's repo" use cases, Docker provides true process isolation with resource limits.

## Decision

Add a `--docker` flag and `[docker]` config block that routes bash tool execution through Docker containers:

1. When `docker.enabled` is true, the bash tool spawns `docker run --rm --network=none --memory=<memory> --cpus=<cpus> -v <workdir>:/work -w /work <image> /bin/sh -c <cmd>` instead of a local shell.
2. Resource limits: `--memory` (default 2g), `--cpus` (default 2), `--network=none` (no network access).
3. The `PathGuard` sandbox stays as the inner gate — Docker is an additional isolation layer, not a replacement.

## Implementation

- `src/cli.rs`: `#[arg(long)] docker: bool` on the `Run` variant.
- `src/shared/mod.rs`: `DockerConfig` struct with `enabled`, `image` (default `ubuntu:24.04`), `memory` (default `2g`), `cpus` (default `2`).
- `src/tools/bash.rs`: `run_docker()` method spawns Docker container with timeout and cancellation. Wired into `run()` when `docker.enabled`.
- `src/tools/mod.rs`: `docker_config` parameter threaded through `all_tools()` → `Bash::new()`.

## Consequences

**Positive:**
- True process isolation for bash commands.
- Resource limits prevent fork bombs and memory exhaustion.
- No network access from within the container.

**Negative:**
- Requires Docker to be installed and running.
- Container startup adds latency (~100-500ms per command).
- Background bash jobs are not containerized (they use the host shell).
- The Docker image must be pulled on first use.
