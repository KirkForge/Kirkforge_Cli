//! OIDC JWT/JWKS verification. Port of `@kirkforge/core-rbac/src/jwt-verify.ts`
//! plus the claim-validation/actor-extraction parts of `index.ts`.
//!
//! Signature verification uses the `jsonwebtoken` crate (ring-based, no openssl).
//! JWKS keys are fetched from the issuer's `.well-known/openid-configuration`
//! (or an explicit `jwks_uri`); local JWKS sets bypass the network for tests.

use crate::error::{AuthError, AuthErrorCode};
use crate::rbac::{resolve_role, Actor, AuthMethod, GroupRoleMapping};
use jsonwebtoken::{
    decode, decode_header,
    jwk::{AlgorithmParameters, Jwk, JwkSet},
    Algorithm, DecodingKey, Validation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// The 10 OIDC algorithms the TS source allows (the policy list, verbatim).
/// No algorithm is dropped from the policy.
pub const ALLOWED_ALGORITHMS: &[&str] = &[
    "RS256", "RS384", "RS512", "ES256", "ES384", "ES512", "PS256", "PS384", "PS512", "EdDSA",
];

/// Algorithms the `jsonwebtoken` verifier can actually verify, per key family.
/// This covers 9 of the 10 policy algorithms: ES512 (P-521) has no variant in
/// `jsonwebtoken` (it bundles `p256`/`p384` only), so ES512 tokens are rejected
/// at header parsing with INVALID_TOKEN.
// ponytail: ceiling — ES512 verifier coverage gap. Upgrade path: add the `p521`
// crate and a manual ES512 verify branch, OR switch to a fuller JOSE crate
// (openssl-backed). Tracked in WO 32.10 / state.md pending. None of the 58
// ported tests exercise ES512 (TS tests RS256 + ES256 only). Adding `p521`
// as a non-dev dependency would inflate the release binary for an alg with
// zero production consumers — deferred until an operator requests ES512.
fn algorithms_for_key(jwk: &Jwk) -> Vec<Algorithm> {
    match &jwk.algorithm {
        // RSASSA-PKCS1-v1_5 AND RSASSA-PSS share the RSA key family.
        AlgorithmParameters::RSA(_) => vec![
            Algorithm::RS256,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::PS384,
            Algorithm::PS512,
        ],
        AlgorithmParameters::EllipticCurve(_) => vec![Algorithm::ES256, Algorithm::ES384],
        AlgorithmParameters::OctetKeyPair(_) => vec![Algorithm::EdDSA],
        // HMAC octet keys are not in the OIDC policy (asymmetric only).
        AlgorithmParameters::OctetKey(_) => vec![],
    }
}

// ── Config & claims ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcConfig {
    pub issuer: String,
    pub audience: String,
    /// Optional explicit JWKS URI (auto-discovered from issuer if absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
    /// Clock-skew tolerance in seconds. Default 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_skew_sec: Option<u64>,
}

impl OidcConfig {
    fn skew_sec(&self, override_skew: Option<u64>) -> u64 {
        override_skew.or(self.clock_skew_sec).unwrap_or(30)
    }
}

/// Audience is either a single string or a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Aud {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: String,
    pub iss: String,
    pub aud: Aud,
    pub exp: i64,
    pub iat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

fn aud_members(aud: &Aud) -> Vec<&str> {
    match aud {
        Aud::One(s) => vec![s.as_str()],
        Aud::Many(v) => v.iter().map(String::as_str).collect(),
    }
}

// ── Claims validation (no signature check) ──────────────────────────────────

/// Validate JWT claims: issuer, audience, expiry, issued-at (all with clock
/// skew tolerance). Signature verification MUST be done by the caller (or via
/// `verify_jwt`). `now_ms` is injectable for deterministic tests.
pub fn validate_jwt_claims(
    claims: &JwtClaims,
    config: &OidcConfig,
    now_ms: Option<i64>,
) -> Result<JwtClaims, AuthError> {
    let now = now_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let skew_ms = config.skew_sec(None) as i64 * 1000;

    if claims.iss != config.issuer {
        return Err(AuthError::new(
            AuthErrorCode::InvalidToken,
            format!(
                "JWT issuer mismatch: expected \"{}\", got \"{}\"",
                config.issuer, claims.iss
            ),
            serde_json::json!({"expected": config.issuer, "actual": claims.iss}),
        ));
    }

    if !aud_members(&claims.aud)
        .iter()
        .any(|a| *a == config.audience)
    {
        return Err(AuthError::new(
            AuthErrorCode::InvalidToken,
            format!("JWT audience mismatch: expected \"{}\"", config.audience),
            serde_json::json!({"expected": config.audience, "actual": claims.aud}),
        ));
    }

    if claims.exp * 1000 < now - skew_ms {
        return Err(AuthError::new(
            AuthErrorCode::InvalidToken,
            "JWT token expired",
            serde_json::json!({"exp": claims.exp, "now_ms": now}),
        ));
    }

    if claims.iat * 1000 > now + skew_ms {
        return Err(AuthError::new(
            AuthErrorCode::InvalidToken,
            "JWT issued-at is in the future",
            serde_json::json!({"iat": claims.iat, "now_ms": now}),
        ));
    }

    Ok(claims.clone())
}

/// Extract an `Actor` from validated JWT claims + OIDC config. Roles are taken
/// from `groups` first, then `roles`; resolved via `resolve_role`.
pub fn actor_from_jwt(
    claims: &JwtClaims,
    _config: &OidcConfig,
    group_mapping: Option<&GroupRoleMapping>,
) -> Result<Actor, AuthError> {
    let groups: Vec<String> = claims
        .groups
        .clone()
        .or_else(|| claims.roles.clone())
        .unwrap_or_default();
    let role = resolve_role(&groups, group_mapping);
    Ok(Actor {
        id: claims.sub.clone(),
        role,
        tenant_id: claims.tenant.clone().unwrap_or_default(),
        auth_method: AuthMethod::Oidc,
        verified_at: crate::rbac::now_iso(),
    })
}

// ── Full JWT verification (signature + JWKS) ────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct VerifyJwtOptions {
    /// Allowable clock skew in seconds. Default 30.
    pub clock_skew_sec: Option<u64>,
    /// Required scopes (space-separated in the token; all must be present).
    pub required_scopes: Vec<String>,
    /// Custom JWKS URI override. If unset, auto-discovered from issuer.
    pub jwks_uri: Option<String>,
    /// HTTP request timeout for JWKS fetch, in milliseconds. Default 5000.
    pub timeout_ms: Option<u64>,
    /// Local JWKS set for testing / pre-fetched keys. Bypasses the network.
    /// Shape: `{ "keys": [ {jwk}, ... ] }`.
    pub jwks_set: Option<Value>,
}

// Global JWKS cache keyed by issuer URL. Faithful to the TS clearJwksCache hook.
static JWKS_CACHE: LazyLock<Mutex<HashMap<String, Arc<Value>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear internal JWKS caches. Useful for testing.
pub fn clear_jwks_cache() {
    JWKS_CACHE.lock().unwrap().clear();
}

fn find_jwk<'a>(jwks: &'a JwkSet, kid: Option<&str>) -> Option<&'a Jwk> {
    if let Some(kid) = kid {
        if let Some(k) = jwks.find(kid) {
            return Some(k);
        }
    }
    jwks.keys.first()
}

fn build_validation(config: &OidcConfig, skew: u64, algorithms: Vec<Algorithm>) -> Validation {
    let mut v = Validation::new(Algorithm::RS256);
    // Per-key-family algorithm list (jsonwebtoken requires every listed alg to
    // share the key's family; a flat cross-family list trips InvalidAlgorithm).
    v.algorithms = algorithms;
    v.leeway = skew;
    v.validate_exp = true;
    v.validate_nbf = false;
    v.iss = Some(HashSet::from([config.issuer.clone()]));
    v.aud = Some(HashSet::from([config.audience.clone()]));
    v
}

fn payload_to_claims(payload: &Value, fallback_aud: &str) -> JwtClaims {
    let aud = match payload.get("aud") {
        Some(Value::String(s)) => Aud::One(s.clone()),
        Some(Value::Array(a)) => Aud::Many(
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
        ),
        _ => Aud::One(fallback_aud.to_string()),
    };
    JwtClaims {
        sub: payload
            .get("sub")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        iss: payload
            .get("iss")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        aud,
        exp: payload.get("exp").and_then(Value::as_i64).unwrap_or(0),
        iat: payload.get("iat").and_then(Value::as_i64).unwrap_or(0),
        roles: payload.get("roles").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        }),
        groups: payload.get("groups").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        }),
        tenant: payload
            .get("tenant")
            .and_then(Value::as_str)
            .map(String::from),
        scope: payload
            .get("scope")
            .and_then(Value::as_str)
            .map(String::from),
    }
}

/// Verify a JWT token's signature using JWKS, then validate claims + scopes.
///
/// Full path: (1) resolve JWKS (local set or OIDC discovery), (2) verify the
/// signature with `jsonwebtoken` (issuer, audience, expiry via `Validation`),
/// (3) map the payload to `JwtClaims`, (4) optional scope check.
pub async fn verify_jwt(
    token: &str,
    config: &OidcConfig,
    _group_mapping: Option<&GroupRoleMapping>,
    options: Option<&VerifyJwtOptions>,
) -> Result<JwtClaims, AuthError> {
    let opts = options.cloned().unwrap_or_default();
    let skew = config.skew_sec(opts.clock_skew_sec);

    let header = decode_header(token)
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let kid = header.kid.as_deref();

    let jwks_value: Arc<Value> = if let Some(local) = opts.jwks_set.clone() {
        Arc::new(local)
    } else {
        fetch_jwks(config, &opts).await?
    };

    let jwks: JwkSet = serde_json::from_value((*jwks_value).clone())
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let jwk = find_jwk(&jwks, kid)
        .ok_or_else(|| AuthError::invalid_token("JWT verification failed: no matching JWKS key"))?;
    let decoding_key = DecodingKey::from_jwk(jwk)
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let algorithms = algorithms_for_key(jwk);
    if algorithms.is_empty() {
        return Err(AuthError::invalid_token(
            "JWT verification failed: unsupported key type",
        ));
    }
    let validation = build_validation(config, skew, algorithms);
    let data = decode::<Value>(token, &decoding_key, &validation)
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;

    let claims = payload_to_claims(&data.claims, &config.audience);

    if !opts.required_scopes.is_empty() {
        let token_scopes: Vec<&str> = claims
            .scope
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let missing: Vec<&str> = opts
            .required_scopes
            .iter()
            .map(String::as_str)
            .filter(|s| !token_scopes.contains(s))
            .collect();
        if !missing.is_empty() {
            return Err(AuthError::invalid_token(format!(
                "Missing required scopes: {}",
                missing.join(", ")
            )));
        }
    }

    Ok(claims)
}

async fn fetch_jwks(config: &OidcConfig, opts: &VerifyJwtOptions) -> Result<Arc<Value>, AuthError> {
    let issuer_key = config.issuer.clone();
    if let Some(cached) = JWKS_CACHE.lock().unwrap().get(&issuer_key) {
        return Ok(cached.clone());
    }
    let jwks_uri = match opts.jwks_uri.clone().or(config.jwks_uri.clone()) {
        Some(u) => u,
        None => discover_jwks_uri(&config.issuer, opts.timeout_ms.unwrap_or(5000)).await?,
    };
    let timeout = opts.timeout_ms.unwrap_or(5000);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout))
        .build()
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let resp = client
        .get(&jwks_uri)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let jwks: Value = resp
        .json()
        .await
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    let arc = Arc::new(jwks);
    JWKS_CACHE.lock().unwrap().insert(issuer_key, arc.clone());
    Ok(arc)
}

/// Discover the JWKS URI from the issuer's `.well-known/openid-configuration`.
/// Falls back to `{issuer}/.well-known/jwks.json` if discovery fails.
async fn discover_jwks_uri(issuer: &str, timeout_ms: u64) -> Result<String, AuthError> {
    let base = issuer.trim_end_matches('/');
    let doc_url = format!("{base}/.well-known/openid-configuration");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| AuthError::invalid_token(format!("JWT verification failed: {e}")))?;
    if let Ok(resp) = client
        .get(&doc_url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(doc) = resp.json::<Value>().await {
                if let Some(uri) = doc.get("jwks_uri").and_then(Value::as_str) {
                    return Ok(uri.to_string());
                }
            }
        }
    }
    Ok(format!("{base}/.well-known/jwks.json"))
}
