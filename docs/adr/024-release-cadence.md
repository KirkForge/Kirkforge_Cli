# ADR-024: Release cadence and semantic versioning

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

KirkForge-Cli ships native binaries for Linux, macOS, and Windows. Until now
releases were ad-hoc. A predictable cadence is needed so users know when to
expect fixes, downstream packagers can plan, and the project can enforce
quality gates (CI green, changelog entries, versioned artifacts) before every
release.

## Decision

### Cadence

- **Minor releases** are published every two weeks on Monday while the project
  is in the `v0.x` series: `v0.2.0`, `v0.3.0`, etc.
- **Patch releases** are published as needed for critical fixes between minors.
- A release is only tagged when `main` CI is fully green.

### Semantic versioning policy

While the major version remains `0` (pre-1.0):

- **Breaking changes** bump the **minor** version (`0.2.0` → `0.3.0`).
- **New features** bump the **minor** version (`0.2.0` → `0.3.0`).
- **Fixes** bump the **patch** version (`0.2.0` → `0.2.1`).

No release in the `v0.x` series will jump to `v1.0.0` without a separate,
explicit decision and ADR update. Once `v1.0.0` is reached, normal SemVer rules
apply (breaking → major, features → minor, fixes → patch).

### Changelog discipline

- `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
- Every PR that changes observable behavior must add a line under
  `## [Unreleased]`.
- A CI check enforces this: the PR must either carry a `changelog:` label or
  add a line that contains the PR number (`#NNN`) to `CHANGELOG.md`.

### Release workflow

- Pushing a `v*.*.*` tag triggers `.github/workflows/release.yml`.
- The workflow builds six targets:
  - `x86_64-unknown-linux-gnu`
  - `x86_64-unknown-linux-musl`
  - `aarch64-unknown-linux-musl`
  - `x86_64-apple-darwin`
  - `aarch64-apple-darwin`
  - `x86_64-pc-windows-msvc`
- The release job depends on the `build` job and only runs after all targets
  succeed.
- `scripts/bump-version.sh` creates the annotated tag, updates `Cargo.toml`,
  refreshes `Cargo.lock`, and splits the `CHANGELOG.md` Unreleased section.

### Install script smoke test

After a release is published, the install script is verified:

```bash
curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | sh
kirkforge --version
```

Expected output contains the released version.

## Consequences

Positive:

- Predictable delivery for users and plugin developers.
- Every release has a documented changelog and passes the full CI matrix.
- SemVer gives clear expectations even in the `v0.x` pre-1.0 period.

Negative:

- Two-week cadence creates pressure to keep PRs small and well-documented.
- The changelog enforcement check may require PR authors to add a label or
  entry, adding a small overhead.

## Implementation notes

- `README.md` § Releases documents the cadence and SemVer policy.
- `docs/RELEASE.md` contains the runbook for maintainers.
- `.github/workflows/release.yml` already includes the Windows target and the
  `needs: build` gate; it is updated to require the `CI` workflow to pass on
  `main` before a tag is created.
- `.github/workflows/ci.yml` gains a `changelog` job that enforces the
  changelog-or-label rule on pull requests.

ponytail: the `v0.2.0` tag is prepared by `scripts/bump-version.sh` but is only
pushed when `main` CI is green and a human confirms. The automated workflow
never creates the tag itself.

upgrade path: existing `v0.1.0` installs continue to work; they receive the
next release via the same install script when the new tag is published.
