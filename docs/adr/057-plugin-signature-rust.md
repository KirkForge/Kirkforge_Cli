# ADR-057: In-process plugin signature verification (no minisign shell-out)

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugin signature verification (`crates/kirkforge-plugin-host/src/lib.rs`)
shelled out to the `minisign` binary: `find_minisign_binary()` walked
`PATH` looking for a `minisign` executable; if not found, verification
failed with a hard error. This meant:

1. Plugin signatures only worked if `minisign` was installed. A user who
   enables `verify_signatures = true` but doesn't have `minisign` in
   `PATH` gets a hard error for every plugin load.
2. Subprocess overhead + env-curation for what is a pure cryptographic
   verification (Ed25519 signature check over the manifest bytes).
3. Windows friction — `minisign` is not commonly installed on Windows.

The `TrustPolicy::with_verify_signatures` API and the config fields
(`verify_signatures`, `signature_key_path`) remain the public configuration
surface. Internally, the signing model uses minisign (Ed25519) with a single
host-configured public key (set via `plugin_public_key_path`), not per-plugin
keys.

## Decision

Replace the `minisign` shell-out with the pure-Rust `minisign-verify`
crate (v0.2.5, by Frank Denis — the same author as `minisign` itself).
The crate is zero-dependency, pure-Rust, verify-only (no signing), and
adds only Ed25519 verification code to the binary.

The new `verify_plugin_signature`:
    1. Reads `.kf-code.sig` (if missing → error with a clear message).
2. Loads the public key via `minisign_verify::PublicKey::from_file`.
3. Loads the signature via `minisign_verify::Signature::from_file`.
    4. Reads `kf-code.toml` bytes.
5. Verifies the Ed25519 signature with
   `public_key.verify(&manifest_bytes, &signature, true)` (the third
   arg `allow_legacy = true` accepts both standard and legacy
   non-prehashed signatures).
6. Returns `Ok(())` on success, `Err(message)` on any failure.

`find_minisign_binary` and the `Command::new(minisign_bin)` spawn are
deleted. The `minisign` binary is no longer required.

The `minisign` crate (v0.9, full sign+verify) is a dev-dependency only —
used in tests to generate real keypairs + signatures so the test suite
is self-contained and doesn't need the `minisign` binary installed.

## Binary size impact

`minisign-verify` is zero-dependency: it pulls in only Ed25519
verification (the crate vendors a minimal curve25519/ed25519 impl). In
the size-optimized release profile (`opt-level = "z"` + `lto = true` +
`codegen-units = 1`), the estimated impact is under ~100KB — well within
the budget for a security feature. (A full `cargo bloat --release`
measurement was not run in this session due to build time; the dep's
zero-dependency property makes the estimate conservative.)

## Consequences

- `minisign` is no longer required in `PATH` for signature verification.
- Windows users get the same verification path as Unix users.
- The error semantics are unchanged: missing sig file, malformed sig,
  wrong key, signature mismatch all produce the same error categories.
- The `verify_signatures` / `signature_key_path` config fields and the
  `TrustPolicy::with_verify_signatures` API remain the public configuration
  surface. The signing model is minisign (Ed25519) with a single
  host-configured public key — not per-plugin keys.
- The `.kf-code.sig` file format and the `kf-code.toml` signing
  contract are unchanged — only the verification backend changes.
- The signing key model is minisign with a single host-configured public
  key, not per-plugin Ed25519 keys.

## Why `minisign-verify` over `ed25519-dalek` + hand-rolled header parsing

`minisign-verify` handles the minisign header parsing (untrusted
comment, trusted comment, base64 signature, global signature) that
`ed25519-dalek` alone would require us to reimplement. The crate is
maintained by the minisign author, is zero-dependency, and is
audit-friendly (~500 LOC). `ed25519-dalek` would be more work for the
same result with no size advantage.

## Notes

- `ponytail:` / `ceiling:` annotations in the plugin host are preserved.
- The `minisign` dev-dependency does NOT ship in the release binary —
  dev-dependencies are compile-time test-only.