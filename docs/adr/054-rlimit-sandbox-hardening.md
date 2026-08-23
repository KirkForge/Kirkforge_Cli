# ADR-054: rlimit sandbox hardening for the non-Docker bash path

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted (WO 27.1 landlock + WO 30.4 seccomp — see amendments below)
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

## WO 27.1 amendment — landlock filesystem confinement (2026-08-11)

The rlimit layer this ADR specifies bounds CPU / address-space / filesize,
but does **not** bound filesystem reach: a model-driven `bash` call could
read/write anything the `kf-code` user could (the review's C1 finding). WO
27.1 closes that gap by applying Linux landlock in the **same `pre_exec`
hook** as the rlimits, in `setup_rlimits` (`src/session/bash_runner/mod.rs`).

- **Default-on for Linux.** The module is compiled unconditionally under
  `cfg(target_os = "linux")` — it is no longer a Cargo feature. There is no
  opt-in; every non-Docker `bash` child gets confined on Linux 5.13+.
- **Fail-closed.** If `restrict_self` errors on a kernel that should support
  it, the spawn returns `Err` and the bash tool reports a failure rather
  than silently running unconfined.
- **Release escape hatch.** `--i-accept-unsandboxed` (previously debug-only
  per WO 24.3) is now available in release builds. When set, a landlock
  `restrict_self` error logs a loud warning and continues unconfined, and
  the PathGuard production refusal (`refuse_if_production_unsandboxed`) is
  bypassed in favour of `warn_if_unsandboxed`. The operator explicitly
  accepts the risk — intended for WSL2 / old-container kernels where
  `restrict_self` trips despite nominally being supported.
- **Allow-list.** The workspace gets full r/w; system dirs get read-only;
  `$HOME` and the XDG dirs get full r/w (cargo / rustup / npm need to write
  there). Operators extend the list with `security.landlock_extra_paths` in
  `config.toml` (array of strings) or `KF_CODE_LANDLOCK_EXTRA_PATHS` (colon-
  separated); those paths get full r/w. See `src/session/bash_runner/landlock.rs`.

## WO 30.4 amendment — seccomp-bpf syscall filter (2026-08-13)

The external review (2026-08-13) named seccomp as the missing OS-isolation
layer: landlock confines the filesystem; seccomp confines the syscall
surface. The original "Do NOT ship seccomp in this pass" decision (above)
was deferred on the grounds that a BPF compiler was too heavy for the
size-optimized binary. That blocker is removed by the `seccompiler` crate
(pure-Rust, no C deps, pulls only `libc`) — small enough to earn its place
behind an opt-in feature.

- **Default-OFF Cargo feature.** `seccomp = ["dep:seccompiler"]`, opt in via
  `--features seccomp`. Compiled only on Linux (`cfg(all(target_os = "linux",
  feature = "seccomp"))`). The feature ships the capability; it is not yet
  default-on because the allowlist needs exercising against real workloads.
- **Allowlist, not denylist.** Each allowed syscall maps to an empty rule
  (unconditional allow); the match action is `Allow`, the mismatch action is
  `Errno(EPERM)` — graceful failure, not `KILL`/SIGSYS. A tool that hits an
  unlisted syscall fails with a clear `EPERM` rather than vanishing.
- **Applied last in the same `pre_exec` hook.** After rlimits + landlock —
  once seccomp is on, only allowlisted syscalls work, so landlock's own
  syscalls must already have run. Fail-closed like landlock: the
  `--i-accept-unsandboxed` escape hatch governs seccomp too.
- **Compile-in-parent / apply-in-pre_exec split.** `SeccompFilter::new` +
  BPF emit allocate a `BTreeMap` and must run in the parent before fork;
  `seccompiler::apply_filter` does only `prctl(PR_SET_NO_NEW_PRIVS)` + the
  `seccomp` syscall (no allocation) and is safe in the async-signal-safe
  `pre_exec` closure. Mirrors the landlock resolve-in-parent /
  apply-in-pre_exec split. `apply_filter` setting `PR_SET_NO_NEW_PRIVS` is a
  desirable side effect: the sandboxed bash cannot gain privileges via setuid.
- **Allowlist scope.** The base list (WO 30.4) covers bash + common tools
  (grep/sed/awk/curl/cargo/node/python). It is augmented with a glibc-startup
  + modern-`at`-variant block (`arch_prctl`, `set_tid_address`,
  `set_robust_list`, `rt_sigreturn`, `mremap`, `newfstatat`, `faccessat`, …)
  without which no dynamically-linked binary can exec. The list is x86_64-
  tuned; aarch64/riscv64 cross-arch + exotic-tool tuning is deferred. Adding
  a syscall is one line in `allowed_syscalls()`. See
  `src/session/bash_runner/seccomp.rs`.

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

- **seccomp BPF filter.** ✅ Shipped opt-in (WO 30.4 amendment, above). The
  `seccomp` Cargo feature compiles a `seccompiler`-based allowlist filter and
  applies it last in the same `pre_exec` window as the rlimits. Remaining
  work: flip default-on after real-workload allowlist tuning (cross-arch
  aarch64/riscv64 coverage; per-arg `SeccompRule` tightening if a misuse
  vector appears).
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