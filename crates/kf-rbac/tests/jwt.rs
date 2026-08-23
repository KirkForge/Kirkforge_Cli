//! Port of `@kirkforge/core-rbac/tests/jwt-verify.test.ts` (12 JWT tests).
//! Test keypairs: RSA-2048 keys are PRECOMPUTED (embedded as PEM/JWK consts)
//! so no RSA keygen runs at test time — RSA-2048 keygen is 10-50s in a debug
//! test binary and nextest runs each test in its own process, so the
//! `OnceLock`-shared key did not actually share across the 8 slow tests.
//! ES256 (P-256) keygen is ~1ms and stays runtime-generated. The JWKS
//! unreachable path uses an injected `JwksResolver` fake (no network).

use std::sync::Arc;
use std::sync::OnceLock;

use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use kf_rbac::{
    clear_jwks_cache, validate_jwt_claims, verify_jwt, Aud, JwksResolver, JwtClaims, OidcConfig,
    VerifyJwtOptions,
};
use p256::pkcs8::EncodePrivateKey;
use rand::rngs::OsRng;
use serde_json::{json, Value};

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn issuer() -> String {
    "https://auth.example.com".to_string()
}

fn config() -> OidcConfig {
    OidcConfig {
        issuer: issuer(),
        audience: "kirkforge".to_string(),
        jwks_uri: None,
        clock_skew_sec: None,
    }
}

// ── RS256 key material (precomputed) ────────────────────────────────────────
//
// Two distinct RSA-2048 keypairs generated offline. The private key is a
// PKCS8 PEM; the JWK carries the public modulus (n) + exponent (e) as
// base64url-no-pad. `EncodingKey::from_rsa_pem` parses the PEM in ~1ms, vs
// `rsa::RsaPrivateKey::new` which takes 10-50s in a debug test binary.
// nextest isolates each test in its own process, so the prior OnceLock only
// shared the key within a single test process — every `*_local_jwks` test
// paid the full keygen cost. Precomputing drops all 8 to <0.1s.

struct RsaKey {
    encoding: EncodingKey,
    jwk: Value,
}

const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDM8zGFrqUHiSJG\n7lwqqJo+55ZoJA0eUjNG6rr3Lyd65Ftky7xHHFtSZrQXgSyVV1ZeHS4I7C7SFcTq\nC0aI19gx/GuOQMCW+XYdrS4v8YtlxvdEwfh9pm4PadxD5561ams+B4o0TJhMyZr5\n0nrbphbFuJE1dO/z8oENZsZUv2PZFPSmtQV6CYzKVN0w5u3wagx7v9p7tl3ZeEp/\nuKgJL/r57TdWFGIDOC8Vlg+kFmcBnJ+9u11/2Opq2e7H7eFKNe5aL5ZjXJIpSSQQ\nY67R2Sq5qdJSj2eHUI0mB2SXM5BjIzrXLqG5zXSZmE5fuve7EWo3a5xGA20cPwNy\nBa0vpp6nAgMBAAECggEBAIkcWZEZmYZFFA1oAXj4etiCjnj1RWF3lJ5pAtPDVCI/\nC3WPZ1IbgmzKEMl4vZ7bYwhrdRS7DUe3EJmTGYkK/bPTLxFn+HAITCDmbvPcIGni\nhiIVsmw9H3xoUdeL6P1tSHmjCa6bX3hfl7JyQYcYwjtckqi0pqMJJZUVoLGpqN7c\nl24lkbYOb/Qax3L1N0pydk1Mn8DA+73/vLkNtGd/8PL8N3prGWhl3pZu6VbuvcFT\nuW1baInU5bXiXU75W3IO8q5R4SwqbEtSFIYlHHtLhXIXH2swANV2PGNr9s5y+fD9\nnRJRlZQVK8cPIJDk1tbEp/C9nIX/e2/Qm862HzUUY2ECgYEA3LAY9pFaQKP7OgIE\nciTkBq1P2416nwdSeFRTgDBR7UNSlAkugeh1aZeSAi2MGmtrK2YARyypxUfnSK+c\nHhZOxalrrzZP/RBgnqs8brGh4l84rv9U70IPrygXaO0n0Al1zHd1GXyEQa/lAgzY\ngKXeMLgkX859U8ixT4FolNKVyeMCgYEA7b5vB5PVTvv9mIvV+ajydUKpL/gbxM5M\nD7rmlWCnigSI35DNALY4L4ZMXNZ1/8sVtyIaBDGdJv0ch62jnAn3PyC0fcirX746\nbgnM32KFtMibTyxeQBeHV+XtBvpDAh6jJIYtAKaadB7FQr90z//nHdAOVbFyDS+E\nAzZpfdkWA20CgYBHh6Rvtukj7oKtaJ38SUzHhUFPDmpiRUNL0TlHYWIMnzeS1+8F\nGE2GLiSbJBw3K/4OkP8iEq3sTcP/YTwe3Ggn1SQcJGSEx9wpUaNC1bx83RRIVGY5\nLpZa1YnQ7p1q5sYRwd5opl1P1S1LHtLFz/1WmTjg/NLOZ0xhUraNFjyKtQKBgFyN\nOxn/EhZKgSHmpikn/SNrDQQwmVbXXMLu5p8WXoKbW1F1RGlXhq3xoT6u+obW36BI\ndUpWqjAobvfewAeZ1ZfMupcRDK4cFxEJXalE6HpFcjizNAnNXxH333tM59MmbCpm\n1ZQgR5aW+AIRGH90xttTSJFRn+3EJqc9gnnMjgZNAoGBANsVDErg35FqLWYEPgnp\nAtkn70Y3pGzfLd5NZ/00lekFy4L+rdTBya8pFPxVj15/vzqL+2VHe/QOZT6BmE+T\njKH4f1FgBjVPWepdwiuj/IeDY7fjHC+uFGHikPUFVyxFrh6tfZ52kloQBYsTDpWS\nPIiopidHMFwQLiReCKqiRFEB\n-----END PRIVATE KEY-----\n";

const TEST_KEY_N_B64URL: &str = "zPMxha6lB4kiRu5cKqiaPueWaCQNHlIzRuq69y8neuRbZMu8RxxbUma0F4EslVdWXh0uCOwu0hXE6gtGiNfYMfxrjkDAlvl2Ha0uL_GLZcb3RMH4faZuD2ncQ-eetWprPgeKNEyYTMma-dJ626YWxbiRNXTv8_KBDWbGVL9j2RT0prUFegmMylTdMObt8GoMe7_ae7Zd2XhKf7ioCS_6-e03VhRiAzgvFZYPpBZnAZyfvbtdf9jqatnux-3hSjXuWi-WY1ySKUkkEGOu0dkquanSUo9nh1CNJgdklzOQYyM61y6huc10mZhOX7r3uxFqN2ucRgNtHD8DcgWtL6aepw";

const ATTACKER_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQDP4OLDUN+DpXbw\nJVYpne+ZrlKRQ8LoEHFpZSA3Jb4i17Ne6SUYLpf3JaP5ne1UCUkpfw2sj17U8Y2a\nLRjLq+ujpKBB21b3L9WOCxluQa87R03cehAakLnO0ENMigtj+9oSeX16jiL69uEu\nC0cnBMOny2n7z9UF2GVIuMDYMwPXn5h32IHRrhk/AK8Jf7TK2/rRZXjZXDHAz74w\nmbW8MTpFsYyC/cYD7EdjuN+wZgy9Ftyxf2wdqgqh4HaOwCSGs6n1DG13yQAI3fPj\ndq5idOimypf7WxbleEGzxuVSru6qfsD9J4nSqUzcCbrW7HPjLSBH06u79fEkK1+0\nD3Te3HtdAgMBAAECggEABglCrGc5xknUtU5wPQ8f+Pdt4Ff2XeS0VlogYFmRNtPK\nmpPshtI7iWqnY0Upsgn+/Nx6misjls1YzkRG9wsL8ZmDKcZjtRPHgLNjzqbLns4I\nPcGxnAPd0VqMybkscX/LqkOq2BcuftkSWtLrAwAJamLmtfAoAF5zOnRa7Sw2DVnA\nnt3w3CdNtl1wWplofACo0jtQufoQXAYesLH9iB9VVW32xmFfjJLE2lLS6519LJ0L\nl1n/CyEXtOscAhaxzjoH6M6wdANQWNRqBetzEokmBJwQk6sKl5a4B/l5BM+oq6p2\n74ezaAM3fwX5z9svpIBafzrUTrPfVWDGPrzkUnxXIQKBgQDlDDEe7+0pIclvvpMQ\nm5lDvIOy2J2NPdyVwlv8IRLrT6sy2D69/zHFVhHA5dVTAbVY58eN48VDFEjVEOmo\nZOmhR9HXF2IgEdXYT2LmCX/ERour3oEn22s/ZNI2M4s/0MO4tyqmpOLbp2u8xOja\nFU9Jke86y6LXKjMjvEnjrz+bRQKBgQDoVv7/lxITACNgHIujUnyVmYEwxqiJDoWP\nGP7fH4Iy4Jhqkhu6Z7IcNLVY2MUVdr5xkuNELcSmdWcTz4i+do/EObnybmx5+eOJ\ncKI+uyF17ihugLhEgxS6YQx+7HePu09U8/oSKpjnjgMtT6BjogMaKc76wTeE48I0\nxOcwrmFVOQKBgQCm+kFhHWYWo1P3i8YoyFZuRCL6oeIR0rRZ1Qw7/VyOgVD8SxtK\nZZ1CEGH271aaIdezzZzz+sWXBlWmRqMgqRiNBA+dL6XQXVA5Vn5x1yD21LsD+7zK\ncrJ3z6dT7jWouyfEJHwKapAbs6zeO+rI+doId0Qg58158IDBn4V6YAsNxQKBgQCy\n+llDIOgOdPvLTRIQhTltsKuBnHc15VbjbfjgfpA4iyU+a0Eq7jiZW80bHRltOGTq\nbqHd4nfrVuNJsoR/XCvRmDpy07eCmwo51OdW9aaIBydkQIoyVNvB24LZv2U29q7d\nHXjVR7U0IwS1gfJm7eX/4JcOOYuANkdjiQ8jRCG8mQKBgQC7E+IDbtK5dpN/E+Zo\nv7UsRQJNtXu0kZxKD5qAqd0lmy03/s3CEAHojCO09MnEZGvzHXWvpZiW+sR6zkTE\nbOi6wLkff7BEX23sHiwNlHD/PhyVdiSQwCT2uumI/mPy4VmT9qcRioX0ZpQnO9QU\nBvDKlZgYCD4kJRG8iHjF8WYaFQ==\n-----END PRIVATE KEY-----\n";

const ATTACKER_KEY_N_B64URL: &str = "z-Diw1Dfg6V28CVWKZ3vma5SkUPC6BBxaWUgNyW-ItezXuklGC6X9yWj-Z3tVAlJKX8NrI9e1PGNmi0Yy6vro6SgQdtW9y_VjgsZbkGvO0dN3HoQGpC5ztBDTIoLY_vaEnl9eo4i-vbhLgtHJwTDp8tp-8_VBdhlSLjA2DMD15-Yd9iB0a4ZPwCvCX-0ytv60WV42VwxwM--MJm1vDE6RbGMgv3GA-xHY7jfsGYMvRbcsX9sHaoKoeB2jsAkhrOp9Qxtd8kACN3z43auYnTopsqX-1sW5XhBs8blUq7uqn7A_SeJ0qlM3Am61uxz4y0gR9Oru_XxJCtftA903tx7XQ";

fn rsa_key_from_pem(pem: &str, n_b64: &str, kid: &str) -> RsaKey {
    let encoding = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("rsa pem parse");
    let jwk = json!({
        "kty": "RSA",
        "n": n_b64,
        "e": "AQAB",
        "kid": kid,
        "use": "sig",
        "alg": "RS256",
    });
    RsaKey { encoding, jwk }
}

// ponytail: precomputed RSA-2048 keys — keygen was the dominant cost
// (10-50s/test in debug, ×8 processes under nextest per-test isolation).
// Ceiling: all RS256 tests share two fixed keypairs; if a test ever needs
// a fresh keypair (e.g. key-rotation coverage), add a new precomputed
// const rather than re-enabling runtime keygen. Upgrade path: a build.rs
// that generates keys into OUT_DIR if a test ever needs per-build keys.

static TEST_KEY: OnceLock<RsaKey> = OnceLock::new();
static ATTACKER_KEY: OnceLock<RsaKey> = OnceLock::new();

fn shared_key() -> &'static RsaKey {
    TEST_KEY.get_or_init(|| rsa_key_from_pem(TEST_KEY_PEM, TEST_KEY_N_B64URL, "test-key-1"))
}

fn attacker_key() -> &'static RsaKey {
    ATTACKER_KEY
        .get_or_init(|| rsa_key_from_pem(ATTACKER_KEY_PEM, ATTACKER_KEY_N_B64URL, "attacker-key"))
}

fn sign(payload: Value, alg: Algorithm, kid: &str, encoding: &EncodingKey) -> String {
    let mut header = Header::new(alg);
    header.kid = Some(kid.to_string());
    encode(&header, &payload, encoding).expect("sign jwt")
}

fn now_sec() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── ES256 key material (runtime — P-256 keygen is ~1ms) ─────────────────────

struct EcKey {
    encoding: EncodingKey,
    jwk: Value,
}

fn gen_ec(kid: &str) -> EcKey {
    let mut rng = OsRng;
    let signing = p256::ecdsa::SigningKey::random(&mut rng);
    let pem = signing
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("ec pem");
    let encoding = EncodingKey::from_ec_pem(pem.as_bytes()).expect("ec encoding key");
    let verifying = signing.verifying_key();
    let point = verifying.to_encoded_point(false);
    let bytes = point.as_bytes();
    let x = &bytes[1..33];
    let y = &bytes[33..65];
    let jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "x": b64url(x),
        "y": b64url(y),
        "kid": kid,
        "use": "sig",
        "alg": "ES256",
    });
    EcKey { encoding, jwk }
}

fn local_jwks(jwk: &Value) -> VerifyJwtOptions {
    VerifyJwtOptions {
        jwks_set: Some(json!({ "keys": [jwk.clone()] })),
        ..Default::default()
    }
}

// ── Fake JWKS resolver (in-memory, instant — no network) ────────────────────
//
// Used by `verify_returns_invalid_token_when_resolver_fails` to exercise the
// "JWKS unreachable → InvalidToken" error mapping without a real DNS lookup +
// connect timeout. The real HTTP path is kept as a separate `#[ignore]`d
// smoke test (`verify_returns_invalid_token_when_jwks_unreachable_network`).

struct FailingJwksResolver;

impl JwksResolver for FailingJwksResolver {
    fn fetch_jwks<'a>(
        &self,
        _config: &'a OidcConfig,
        _opts: &'a VerifyJwtOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Arc<Value>, kf_rbac::AuthError>> + Send + 'a>,
    > {
        Box::pin(async {
            Err(kf_rbac::AuthError::invalid_token(
                "JWT verification failed: JWKS resolver unreachable (fake)",
            ))
        })
    }
}

// ── verifyJwt (9 tests) ─────────────────────────────────────────────────────

#[tokio::test]
async fn verify_accepts_valid_jwt_local_jwks() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now,"groups":["developers"]}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let claims = verify_jwt(&token, &config(), None, Some(&local_jwks(&k.jwk)))
        .await
        .expect("valid token");
    assert_eq!(claims.sub, "user-1");
    assert_eq!(claims.iss, issuer());
    assert_eq!(claims.groups, Some(vec!["developers".to_string()]));
}

#[tokio::test]
async fn verify_rejects_wrong_issuer_local_jwks() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":"https://evil.com","aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let err = verify_jwt(&token, &config(), None, Some(&local_jwks(&k.jwk)))
        .await
        .unwrap_err();
    assert_eq!(err.code, kf_rbac::AuthErrorCode::InvalidToken);
}

#[tokio::test]
async fn verify_rejects_wrong_audience_local_jwks() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":issuer(),"aud":"wrong-audience","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    assert!(
        verify_jwt(&token, &config(), None, Some(&local_jwks(&k.jwk)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn verify_rejects_expired_token_local_jwks() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":issuer(),"aud":"kirkforge","exp":now-300,"iat":now-3600}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let cfg = OidcConfig {
        clock_skew_sec: Some(10),
        ..config()
    };
    assert!(verify_jwt(&token, &cfg, None, Some(&local_jwks(&k.jwk)))
        .await
        .is_err());
}

#[tokio::test]
async fn verify_resolves_roles_from_group_mapping() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"admin-user","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now,"groups":["platform-admins"]}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let mapping = kf_rbac::GroupRoleMapping(
        [("platform-admins".to_string(), kf_rbac::Role::Admin)]
            .into_iter()
            .collect(),
    );
    let claims = verify_jwt(&token, &config(), Some(&mapping), Some(&local_jwks(&k.jwk)))
        .await
        .expect("valid");
    assert_eq!(claims.groups, Some(vec!["platform-admins".to_string()]));
}

#[tokio::test]
async fn verify_rejects_token_signed_with_wrong_key_local_jwks() {
    clear_jwks_cache();
    let right = shared_key();
    let wrong = attacker_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"attacker","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "test-key-1",
        &wrong.encoding,
    );
    assert!(
        verify_jwt(&token, &config(), None, Some(&local_jwks(&right.jwk)))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn verify_enforces_required_scopes_local_jwks() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now,"scope":"read write"}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let opts_ok = VerifyJwtOptions {
        required_scopes: vec!["read".to_string()],
        ..local_jwks(&k.jwk)
    };
    assert!(verify_jwt(&token, &config(), None, Some(&opts_ok))
        .await
        .is_ok());

    let opts_fail = VerifyJwtOptions {
        required_scopes: vec!["read".to_string(), "admin".to_string()],
        ..local_jwks(&k.jwk)
    };
    let err = verify_jwt(&token, &config(), None, Some(&opts_fail))
        .await
        .unwrap_err();
    assert!(err.message.contains("Missing required scopes"));
}

#[tokio::test]
async fn verify_returns_invalid_token_when_resolver_fails() {
    // In-process: the injected FailingJwksResolver returns InvalidToken
    // instantly, proving the error mapping without a real DNS lookup +
    // connect timeout. The real HTTP path is covered by the `#[ignore]`d
    // `verify_returns_invalid_token_when_jwks_unreachable_network` smoke
    // test below.
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":"https://auth-unreachable.example.com","aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let cfg = OidcConfig {
        issuer: "https://auth-unreachable.example.com".to_string(),
        audience: "kirkforge".to_string(),
        jwks_uri: None,
        clock_skew_sec: None,
    };
    let opts = VerifyJwtOptions {
        resolver: Some(Arc::new(FailingJwksResolver)),
        ..Default::default()
    };
    let err = verify_jwt(&token, &cfg, None, Some(&opts))
        .await
        .unwrap_err();
    assert_eq!(err.code, kf_rbac::AuthErrorCode::InvalidToken);
    assert!(err.message.contains("JWT verification failed"));
}

/// Real-network smoke test for the HTTP JWKS path. Gated `#[ignore]` so it
/// does not run in the default gate (it waits for a real DNS lookup + connect
/// timeout against an unreachable host). Run with
/// `cargo nextest run -p kf-rbac --run-ignored only --exact ...` or a nightly
/// profile. The in-process equivalent is
/// `verify_returns_invalid_token_when_resolver_fails` above.
#[tokio::test]
#[ignore = "real-network JWKS smoke test — waits for DNS+connect timeout; run via --run-ignored"]
async fn verify_returns_invalid_token_when_jwks_unreachable_network() {
    clear_jwks_cache();
    let k = shared_key();
    let now = now_sec();
    let token = sign(
        json!({"sub":"user-1","iss":"https://auth-unreachable.example.com","aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "test-key-1",
        &k.encoding,
    );
    let cfg = OidcConfig {
        issuer: "https://auth-unreachable.example.com".to_string(),
        audience: "kirkforge".to_string(),
        jwks_uri: None,
        clock_skew_sec: None,
    };
    let err = verify_jwt(&token, &cfg, None, None).await.unwrap_err();
    assert_eq!(err.code, kf_rbac::AuthErrorCode::InvalidToken);
    assert!(err.message.contains("JWT verification failed"));
}

#[tokio::test]
async fn verify_accepts_es256_token_local_jwks() {
    clear_jwks_cache();
    let k = gen_ec("test-ec-key");
    let now = now_sec();
    let token = sign(
        json!({"sub":"ec-user","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::ES256,
        "test-ec-key",
        &k.encoding,
    );
    let claims = verify_jwt(&token, &config(), None, Some(&local_jwks(&k.jwk)))
        .await
        .expect("valid es256");
    assert_eq!(claims.sub, "ec-user");
}

// ── ES512 gap (WO 32.10) ────────────────────────────────────────────────────

/// ES512 (P-521 ECDSA) is in the `ALLOWED_ALGORITHMS` policy list but the
/// `jsonwebtoken` verifier has no ES512 variant — it bundles `p256`/`p384`
/// only. Closing the gap requires either the `p521` crate as a non-dev
/// dependency (manual JWK→verifying-key + DER signature decode) or a fuller
/// JOSE crate. Both inflate the release binary for an alg with zero
/// production consumers, so the gap is deferred until an operator
/// requests ES512. Tracked in state.md pending (WO 32.10 is closed).
#[tokio::test]
#[ignore = "ES512 verifier not implemented — needs p521 non-dev dep; tracked in state.md pending"]
async fn es512_verifier_gap_is_documented() {
    clear_jwks_cache();
    let jwk = json!({
        "kty": "EC",
        "crv": "P-521",
        "x": "Aec0mBhZGjPl1c1c3jX1c1c1c1c1c1c1c1c1c1c1c1c",
        "y": "Ad1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c",
        "kid": "es512-key",
        "use": "sig",
        "alg": "ES512",
    });
    let now = now_sec();
    let k = shared_key();
    let token = sign(
        json!({"sub":"es512-user","iss":issuer(),"aud":"kirkforge","exp":now+3600,"iat":now}),
        Algorithm::RS256,
        "es512-key",
        &k.encoding,
    );
    let err = verify_jwt(&token, &config(), None, Some(&local_jwks(&jwk)))
        .await
        .unwrap_err();
    assert_eq!(err.code, kf_rbac::AuthErrorCode::InvalidToken);
    assert!(
        err.message.contains("unsupported key type")
            || err.message.contains("JWT verification failed"),
        "ES512 JWK should be rejected at the key-family gate, got: {}",
        err.message
    );
}

// ── validateJwtClaims (3 tests) ─────────────────────────────────────────────

fn claim(sub: &str, exp: i64, iat: i64) -> JwtClaims {
    JwtClaims {
        sub: sub.to_string(),
        iss: "https://auth.example.com".to_string(),
        aud: Aud::One("kirkforge".to_string()),
        exp,
        iat,
        roles: None,
        groups: None,
        tenant: None,
        scope: None,
    }
}

#[test]
fn validate_accepts_valid_claims() {
    let now = now_sec();
    let c = claim("user-1", now + 3600, now);
    assert!(validate_jwt_claims(&c, &config(), None).is_ok());
}

#[test]
fn validate_rejects_expired_claims() {
    let now = now_sec();
    let c = claim("user-1", now - 300, now - 3600);
    assert!(validate_jwt_claims(&c, &config(), None).is_err());
}

#[test]
fn validate_rejects_wrong_issuer() {
    let now = now_sec();
    let mut c = claim("user-1", now + 3600, now);
    c.iss = "https://evil.com".to_string();
    assert!(validate_jwt_claims(&c, &config(), None).is_err());
}
