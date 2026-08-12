//! Timing-safe API-key authentication. Mirrors the TS `actorFromApiKey`.
//! Uses `subtle::ConstantTimeEq` with the same right-aligned padding so the
//! length check does not leak key length via timing.

use crate::error::{AuthError, AuthErrorCode};
use crate::rbac::{now_iso, Actor, AuthMethod, Role};
use subtle::ConstantTimeEq;

/// Validate a bearer token against a static API key using constant-time
/// comparison. Returns an `Actor` with the specified role on success.
///
/// Defaults mirror the TS signature: `role = operator`, `tenant = ""`. Rust has
/// no default arguments, so callers pass them explicitly.
pub fn actor_from_api_key(
    token: &str,
    expected_api_key: &str,
    role: Role,
    tenant_id: &str,
) -> Result<Actor, AuthError> {
    if token.is_empty() || expected_api_key.is_empty() {
        return Err(AuthError::new(
            AuthErrorCode::Unauthorized,
            "Missing token or API key",
            serde_json::Value::Object(serde_json::Map::new()),
        ));
    }

    // Timing-safe comparison: pad both buffers to equal length so the length
    // check does not leak key length via timing. Right-aligned (matches TS).
    let token_buf = token.as_bytes();
    let key_buf = expected_api_key.as_bytes();
    let max_len = token_buf.len().max(key_buf.len());
    let mut padded_token = vec![0u8; max_len];
    let mut padded_key = vec![0u8; max_len];
    let t_off = max_len - token_buf.len();
    let k_off = max_len - key_buf.len();
    padded_token[t_off..].copy_from_slice(token_buf);
    padded_key[k_off..].copy_from_slice(key_buf);
    let ok: bool = padded_token.ct_eq(&padded_key).into();
    if !ok {
        return Err(AuthError::new(
            AuthErrorCode::InvalidToken,
            "Invalid token",
            serde_json::Value::Object(serde_json::Map::new()),
        ));
    }

    Ok(Actor {
        id: format!("api-key:{}", role.as_str()),
        role,
        tenant_id: tenant_id.to_string(),
        auth_method: AuthMethod::ApiKey,
        verified_at: now_iso(),
    })
}
