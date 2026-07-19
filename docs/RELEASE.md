# Release runbook

This document describes how KirkForge-Cli releases are made, versioned, and
verified.

## Cadence

- **Minor releases** (`v0.2.0`, `v0.3.0`, …) ship every two weeks on Monday
  while the project remains in the `v0.x` series.
- **Patch releases** (`v0.2.1`, `v0.2.2`, …) ship as needed for critical fixes
  between minors.
- A release is only published when `main` CI is fully green.

## Semantic versioning policy

Because the project is still pre-1.0:

- **Breaking changes** bump the **minor** version.
- **New features** bump the **minor** version.
- **Fixes** bump the **patch** version.

There is no plan to bump to `v1.0.0` from this runbook; that requires a
separate decision and ADR update.

## Before releasing

1. Ensure the `CHANGELOG.md` `## [Unreleased]` section documents every
   behavior-changing PR merged since the last release.
2. Verify the local CI gate passes:
   ```bash
   scripts/ci-local.sh
   ```
3. Confirm the `CI` workflow on `main` is green.
4. Decide the next version number based on the SemVer policy above.

## Creating the release

Use the bump script:

```bash
scripts/bump-version.sh 0.2.0
```

This:

- updates the version in `Cargo.toml`,
- refreshes `Cargo.lock`,
- splits the Unreleased section of `CHANGELOG.md` into a versioned section,
- commits the changes,
- creates an annotated `v0.2.0` tag.

Then push the tag:

```bash
git push origin v0.2.0
```

Pushing the tag triggers `.github/workflows/release.yml`, which builds the
following targets and attaches the archives to a GitHub release:

- `x86_64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

## After the release

1. Wait for the `Release` workflow to finish.
2. Run the install-script smoke test on a clean Linux/macOS machine:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | sh
   kirkforge --version
   ```
   Expected output: the released version (e.g., `kirkforge 0.2.0`).
3. Verify Windows artifacts exist on the release page and contain the five
   binaries (`kirkforge.exe`, `kfd.exe`, `plugin3.exe`, `stratum.exe`,
   `kirkforge-video.exe`), the bundled `plugins/`, and the Node SDK under
   `npm/kirkforge-plugin/`.
4. If anything fails, delete the release and tag, fix `main`, and start over.

## Changelog enforcement in CI

Every PR that changes observable behavior must either:

- add a `changelog:` label, or
- add a line to `CHANGELOG.md` under `## [Unreleased]` that references the PR
  number (`#NNN`).

`.github/workflows/ci.yml` runs a `changelog` job that checks this rule.

## Emergency patch release

For a critical fix:

1. Land the fix on `main` with a changelog entry.
2. Wait for CI to go green.
3. Run `scripts/bump-version.sh X.Y.Z+1` and push the resulting tag.

Do **not** tag a release on red CI.
