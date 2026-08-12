# Lessons — WO 29.5 (kf-rbac port)

## What I learned about this codebase / ecosystem
- **`jsonwebtoken` (9.x–11.x) has NO `ES512` variant** — it bundles `p256`/`p384` only, never `p521`. So ES512 (one of the 10 OIDC algorithms in the TS `ALLOWED_ALGORITHMS`) cannot be represented in `Algorithm` or verified. The Step-2 workorder note "jsonwebtoken (RS/ES/EdDSA but not PS)" was wrong about PS (PS* IS supported) but missed the real gap: ES512. Keep ES512 in the policy string list (no drop), reject ES512 tokens at `decode_header` with INVALID_TOKEN. Disclosed as a deferral.
- **`jsonwebtoken` `Validation::algorithms` must all share the key's family.** `verify_signature` (decoding.rs:218) loops every listed alg and errors `InvalidAlgorithm` if any != `key.family()`. A flat cross-family list (RS+ES+Ed together) fails even for a valid RSA token. Fix: derive `algorithms` **per JWK family** (`algorithms_for_key`). Side benefit: tighter alg-confusion prevention.
- **`DecodingKey::from_jwk` (9.3.1) takes `&Jwk`, not a string.** Deserialize the JWKS JSON into `jsonwebtoken::jwk::JwkSet` and use `JwkSet::find(kid)` — cleaner than string round-tripping.
- **`Validation` fields `iss`/`aud` are `Option<HashSet<String>>`** (not `String` / `Vec`). Use `HashSet::from([s])`.
- **`rsa::RsaPublicKey` exposes `n()`/`e()` via the `PublicKeyParts` trait** (private fields, trait methods) — `use rsa::traits::PublicKeyParts;`. `RefCell::clone` clones the inner value, NOT a shared handle — use `Rc<RefCell<_>>` (or `Arc<Mutex<_>>`) to share a capture store between a closure and its test.
- **clippy `should_implement_trait`** flags inherent `from_str` methods — implement `std::str::FromStr` instead; tests use `x.parse::<Role>()` (no trait import needed).
- Repo conventions reaffirmed: `ponytail:`/`ceiling:`/`upgrade path:` comments, defer-disclosure mandatory, doc-sync (TECHNICAL.md crate tree+table) in the same commit.

## What I'd do differently
- Read the chosen crate's `Algorithm` enum BEFORE writing the algorithm-list code — would have caught ES512 and the family-match rule up front, saving one debug cycle.
- RSA-2048 keygen in debug mode is ~60s/key (the "wrong key" test does 2 → ~120s). For faster JWT test cycles, could downgrade test keys to RSA-1024 or cache one keypair. Left as-is (correctness > test speed; release builds are fine).

## ES512 defer note
Disclosed in workorder status + state.md + TECHNICAL.md + CHANGELOG. Remaining: `p521` crate + manual branch, or openssl-backed JOSE.
