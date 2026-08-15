//! Port of `@kirkforge/core-rbac/tests/jwt-verify.test.ts` (12 JWT tests).
//! Test keypairs are generated at runtime (rsa + p256 dev-deps) — no network.

use std::sync::OnceLock;

use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use kf_rbac::{
    clear_jwks_cache, validate_jwt_claims, verify_jwt, Aud, JwtClaims, OidcConfig, VerifyJwtOptions,
};
use rand::rngs::OsRng;
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
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

// ── RS256 key material ──────────────────────────────────────────────────────

struct RsaKey {
    encoding: EncodingKey,
    jwk: Value,
}

fn gen_rsa(kid: &str) -> RsaKey {
    let mut rng = OsRng;
    let priv_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let pem = priv_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("rsa pem");
    let encoding = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("encoding key");
    let pub_key = priv_key.to_public_key();
    let n = pub_key.n().to_bytes_be();
    let e = pub_key.e().to_bytes_be();
    let jwk = json!({
        "kty": "RSA",
        "n": b64url(&n),
        "e": b64url(&e),
        "kid": kid,
        "use": "sig",
        "alg": "RS256",
    });
    RsaKey { encoding, jwk }
}

// ponytail: RSA-2048 keygen is ~7-40s; share one key across all JWT tests via OnceLock.
// Ceiling: all RS256 tests share one keypair — fine for verify-only tests; if any test
// mutates the key or depends on a fresh keypair, add a separate OnceLock.
// Upgrade path: per-test keys if isolation is ever required.
static TEST_KEY: OnceLock<RsaKey> = OnceLock::new();
static ATTACKER_KEY: OnceLock<RsaKey> = OnceLock::new();

fn shared_key() -> &'static RsaKey {
    TEST_KEY.get_or_init(|| gen_rsa("test-key-1"))
}

fn attacker_key() -> &'static RsaKey {
    ATTACKER_KEY.get_or_init(|| gen_rsa("attacker-key"))
}

fn sign(payload: Value, alg: Algorithm, kid: &str, encoding: &EncodingKey) -> String {
    let mut header = Header::new(alg);
    header.kid = Some(kid.to_string());
    encode(&header, &payload, encoding).expect("sign jwt")
}

fn now_sec() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── ES256 key material ──────────────────────────────────────────────────────

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
async fn verify_returns_invalid_token_when_jwks_unreachable() {
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
