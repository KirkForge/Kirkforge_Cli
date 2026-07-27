# ADR-054: rlimit sandbox hardening for the non-Docker bash path

- **Status:** Accepted
- **Date:** 2026-07-26

## Context

The bash sandbox was path-based only — `PathGuard`, the deny-list,
permission rules, and `bash_sandbox_workdir`. ADR-036 added Docker
execution mode (`--docker`) for full process isolation with `--memory`
and `--cpus` caps, but Docker is heavy: it requires the daemon installed
and running, adds 100–500ms of container startup per command, and is
unavailable on potato hardware (the project's stated target).

Outside Docker, the bash tool ran child shells with the parent's full
process privileges. A user who didn't want Docker overhead had no
lightweight sandbox hardening — a runaway `:(){ :|:& };:` fork bomb,
a `cat /dev/urandom > /tmp/x` disk fill, or a `mmap`-the-world memory
blowup could exhaust host resources before the tool's wall-clock
timeout fired.

WO 9.8 asked for a lightweight hardening layer that runs without
Docker: a seccomp BPF filter (syscall allow-list) plus rlimits.

## Decision

### 1. Ship rlimits only. Do NOT ship seccomp in this pass.

seccomp requires a BPF compiler — either `libseccomp` bindings (a C
library + FFI) or a hand-rolled BPF assembler in Rust. Either path adds
weight that the size-optimized binary cannot afford:

- `libseccomp` pulls in a C runtime dep, breaking the static-binary
  story and complicating the cross-compile in `Cross.toml`.
- A hand-rolled BPF assembler (e.g. the `seccomp` or `seccompiler`
  crate) is ~5–10 KB of Rust but needs a per-arch syscall number table
  and is fragile across kernel versions. It also can't be tested
  meaningfully in CI without running the binary as root under a real
  seccomp filter, which the test runner isn't set up for.

rlimits, by contrast, are a single `setrlimit` syscall per resource,
have no per-arch table, are in the kernel ABI since POSIX, and are
already available via `libc = "0.2"` (a direct dep since the daemon
work — `Cargo.toml:69`). The failure modes the WO names (CPU burn,
memory blowup, disk fill) are all covered by rlimits:

- `RLIMIT_CPU` → SIGXCPU on exhaustion, SIGKILL after a one-second
  grace period if uncaught.
- `RLIMIT_AS` → ENOMEM from `malloc`/`mmap`/`brk` past the cap.
- `RLIMIT_FSIZE` → SIGXFSZ on `write` past the cap.

seccomp is documented as future work below. The rlimit seam is the
right place to add it later: a `setup_seccomp` hook alongside
`setup_rlimits` in `bash_runner/mod.rs` can apply a BPF filter in the
same `pre_exec` window without touching the bash tool surface.

### 2. Apply rlimits in a `pre_exec` hook (post-fork, pre-exec).

`setrlimit` is per-process. Setting it on the parent before spawning
the child would cap the *agent* process, not the child — and the cap
would persist after the child exits, degrading the agent's own malloc
pool. Setting it in a `pre_exec` hook (the same window
`setup_process_group` uses for `setpgid`) installs the limits on the
child only, between `fork` and `exec`. This is the only async-signal-
safe place to do it.

`setrlimit` failures in `pre_exec` are ignored: a failed `setrlimit`
is a degraded sandbox, not a crash, and exec should still proceed so
the user sees a clear error from the child rather than a silent spawn
failure. This matches the existing `setpgid` precedent in
`process_group.rs`.

### 3. Add `SandboxConfig` to `SecurityConfig`, mirroring `DockerConfig`.

```rust
pub struct SandboxConfig {
    pub harden: bool,              // default false
    pub cpu_limit_secs: u64,       // default 300 (5 min)
    pub memory_limit_mb: u64,      // default 2048 (2 GiB)
    pub filesize_limit_mb: u64,   // default 512 (MiB)
}
```

Defaults are deliberately generous so a normal `cargo build` / `cargo
test` completes but a runaway loop is contained. The `harden` flag is
gated (default false) so existing sessions see no behaviour change;
the operator opts in via `--harden` or `[security.sandbox] harden = true`
in `config.toml`.

When `docker.enabled` is true, the sandbox config is ignored — Docker
already enforces `--memory` and `--cpus`, and layering rlimits on top
of a containerized process is redundant.

### 4. `--harden` CLI flag, mirroring `--docker`.

`#[arg(long)] harden: bool` on the `Run` variant in `src/cli.rs`,
threaded through `RunArgs` in `src/main/mod.rs`, setting
`config.security.sandbox.harden = true` when set. Like `--docker`, it
is a transient runtime override — not persisted to `config.toml`.

### 5. Windows: no-op with a one-shot warning.

rlimits are a Unix-only concept. Windows has job objects (a separate
API surface), but wiring them is out of scope for this WO and would
add a Windows-specific code path that the project's Windows-parity ADR
(ADR-025) doesn't require. On Windows, `setup_rlimits` is a no-op and
prints a one-shot `eprintln!` warning so a user who enables `--harden`
on Windows knows it's a no-op, not a silent no-op. The warning uses a
`OnceLock` so it fires once per process, not once per command.

## Implementation

- `src/shared/mod.rs`: `SandboxConfig` struct with `#[serde(default =
  "...")]` on each field, mirroring `DockerConfig`'s pattern.
- `src/shared/config/security.rs`: `pub sandbox: SandboxConfig` on
  `SecurityConfig` + `Default` impl.
- `src/cli.rs`: `#[arg(long)] harden: bool` on `Run`.
- `src/main/mod.rs`: `harden` in `RunArgs`, sets
  `config.security.sandbox.harden = true`, threads
  `config.security.sandbox.clone()` into `all_tools()`.
- `src/tools/mod.rs`: `sandbox_config: SandboxConfig` parameter on
  `all_tools()`, passed to `Bash::new()`.
- `src/tools/bash.rs`: `Bash` holds `SandboxConfig`; non-Docker path
  passes `Some(&self.sandbox_config)` to `run_shell_with_token`.
- `src/session/bash_runner/mod.rs`: `setup_rlimits(cmd, cfg)` helper
  (Unix: `pre_exec` hook with three `setrlimit` calls; non-Unix:
  no-op + warning). `run_shell_with_token` gains an
  `Option<&SandboxConfig>` parameter; `run_shell` passes `None`.
- `src/tui/commands/persona.rs`, `src/tools/task.rs`,
  `src/session/bench.rs`: updated `all_tools()` call sites.

## Consequences

Positive:

- A user who doesn't want Docker overhead gets a lightweight sandbox
  hardening layer that costs three syscalls per spawn and zero new
  dependencies. The binary size impact is negligible (a few hundred
  bytes of code, no new crate).
- The three failure modes the WO names — CPU burn, memory blowup, disk
  fill — are contained at the kernel level, before the tool's
  wall-clock timeout fires. SIGXCPU/SIGXFSZ/ENOMEM arrive from the
  kernel, not from a userspace timer.
- The `SandboxConfig` seam is the future home for seccomp: a
  `setup_seccomp` hook can sit alongside `setup_rlimits` without
  changing the bash tool surface.
- Defaults are gated (`harden: false`), so existing sessions see no
  behaviour change. The operator opts in explicitly.

Negative:

- rlimits are coarse. `RLIMIT_AS` caps the *virtual* address space, not
  RSS — a child that `mmap`s a huge file lazily won't be charged until
  it touches the pages, and a child that uses `madvise(MADV_DONTNEED)`
  to release pages can evade the cap. `RLIMIT_CPU` is CPU seconds, not
  wall-clock — a child that `sleep`s for an hour uses ~0 CPU seconds.
  The wall-clock tool timeout (`timeout` arg on the bash tool) remains
  the primary bound on elapsed time; rlimits bound the *resource*
  consumption, not the duration.
- rlimits are per-process, not per-cgroup. A fork bomb that hits
  `RLIMIT_NPROC` (not configured here — `RLIMIT_CPU` and `RLIMIT_AS`
  are the bounds that matter for a single shell) could spawn many
  small children before the parent's CPU cap fires. The existing
  process-group kill on timeout (`kill_process_group`) reaches all
  descendants, but a fork bomb that evades the rlimits would still
  consume resources until the wall-clock timeout. A future WO could
  add `RLIMIT_NPROC` to bound fork-bomb breadth, but it's not in this
  WO's scope.
- `RLIMIT_FSIZE` caps the size of any single file the child creates,
  not total disk usage. A child that creates a thousand 512-MiB files
  (under the per-file cap) can still fill the disk. This is a known
  limitation; the per-file cap is the cheap bound that catches the
  common `cat /dev/urandom > /tmp/x` case.
- Windows users get no hardening. This is acceptable for WO 9.8; a
  future WO could add Windows job objects with the same `SandboxConfig`
  seam.

## Tests

- `tools::bash::tests::bash_harden_kills_cpu_burn_with_sigxcpu`
  (`#[cfg(unix)] #[ignore]`) — spawns `while :; do :; done` with
  `harden=true` and `cpu_limit_secs=1`, asserts the child is killed
  (Failure outcome with `exit_code: None` or `-1`, signal-killed)
  within 25 seconds — well under the 30-second tool timeout, proving
  the rlimit fired and the child did not run for the full timeout.
  The test is `#[ignore]` by default because it relies on `setrlimit`
  behaviour that is only meaningful on a real Unix host with a sane
  scheduler. Run with `cargo test -- --ignored` to exercise it.

## Future work

- **seccomp BPF filter.** A `setup_seccomp` hook in
  `bash_runner/mod.rs`, sitting alongside `setup_rlimits` in the same
  `pre_exec` window. The filter would allow-list the syscalls a shell
  needs (`read`, `write`, `open`, `close`, `fork`, `exec`, `wait`,
  `exit`, `mmap`, `munmap`, `brk`, `sigaction`, `pipe`, `dup`, the
  `fcntl` family, `stat`/`fstat`/`lstat`) and deny everything else
  (`ptrace`, `mount`, `reboot`, `kexec_load`, the `perf_event_open`
  side-channel family). This is the syscall-level bound that rlimits
  cannot provide. The blocker is the BPF compiler dependency; a
  hand-rolled BPF assembler (no C dep, ~5 KB of Rust) is the path
  that fits the size-optimized binary.
- **`RLIMIT_NPROC`.** Bound the number of child processes a single
  shell can fork, catching fork bombs that evade `RLIMIT_CPU` by
  distributing work across many short-lived children.
- **Per-tool rlimit overrides.** Allow the operator to set
  different rlimits for `bash` vs `bash_status` vs the scheduled-job
  daemon, rather than the single global `SandboxConfig`.
- **Windows job objects.** Map `SandboxConfig` to Windows job-object
  `JOBJECT_CPU_RATE_CONTROL` + `JOB_OBJECT_LIMIT_PROCESS_MEMORY` +
  `JOB_OBJECT_LIMIT_JOB_MEMORY` so `--harden` does something useful
  on Windows.
- **Telemetry.** Emit a metric when SIGXCPU/SIGXFSZ/ENOMEM fires so the
  operator can see how often the sandbox is actually catching a
  runaway command, and tune the defaults accordingly.