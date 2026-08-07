# ADR-068: Remove Standalone Subsystem CLIs

- **Status:** Accepted
- **Date:** 2026-08-07

## Context

`kf-budget-cli` and `kf-compress-cli` existed as standalone CLI crates for
the budget and stratum subsystems. They shipped their own `main()` entry
points and were workspace members.

After the Stratum and Plugin3 fold-ins (ADRs 046, 047), budget and stratum
tools are compiled into the `kf-code` binary via the `budget` and `stratum`
feature flags. The standalone CLIs duplicated tool dispatch that the in-process
path already handles.

## Decision

Remove `kf-budget-cli` and `kf-compress-cli` from the workspace. Delete their
crate directories entirely.

Budget and stratum tools are now available only via:
- An active `kf-code` session (in-process tools)
- The `kirkforge` CLI subcommands when relevant

The standalone CLIs added no value — the in-process tools are the primary
interface, and the shell-plugin fallback path (which the standalone CLIs
resembled) has also been removed.

## Consequences

- No standalone `kf-budget-cli` or `kf-compress-cli` binaries.
- Smaller workspace: two fewer crates to compile and track.
- Budget and stratum functionality remains fully available behind feature
  flags in `kf-code`.
