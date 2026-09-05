//! Timing-safe API-key authentication. Mirrors the TS `actorFromApiKey`.
//! Uses `subtle::ConstantTimeEq` with fixed-size padding so neither the
//! allocation size nor the comparison width leak key length via timing.

use crate::error::{AuthError, AuthErrorCode};
use crate::rbac::{now_iso, Actor, AuthMethod, Role};
use subtle::ConstantTimeEq;

// ponytail: 256-byte fixed ceiling for constant-time padding. Allocating a
// fixed buffer (not max(token,key) len) closes the max_len timing leak the
// WO 50.09 audit flagged. 256 is generous for API keys (typically 32-64
// chars). Upgrade path: if key lengths ever vary wildly past 256, switch to
// `constant_time_eq` with fixed-size `[u8; N]` buffers, or hash both inputs
// to a fixed-length digest and compare the digests.

/// Fixed comparison buffer width. Both token and key are padded (or
/// truncated) to this length before the constant-time compare, so the
/// allocation and comparison time are independent of either input's length.
const CT_BUF_LEN: usize = 256;

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

    // Timing-safe comparison: pad both buffers to a FIXED width so the
    // allocation size and comparison width do not leak which string is
    // longer. If either input exceeds CT_BUF_LEN it is truncated and the
    // comparison is forced to mismatch (a too-long token can never match a
    // too-long-or-short key, and vice versa).
    let token_buf = token.as_bytes();
    let key_buf = expected_api_key.as_bytes();
    let token_trunc = token_buf.len() > CT_BUF_LEN;
    let key_trunc = key_buf.len() > CT_BUF_LEN;
    let mut padded_token = [0u8; CT_BUF_LEN];
    let mut padded_key = [0u8; CT_BUF_LEN];
    // Right-align (matches TS semantics): short inputs sit at the tail.
    let t_off = CT_BUF_LEN - token_buf.len().min(CT_BUF_LEN);
    let k_off = CT_BUF_LEN - key_buf.len().min(CT_BUF_LEN);
    padded_token[t_off..].copy_from_slice(&token_buf[..token_buf.len().min(CT_BUF_LEN)]);
    padded_key[k_off..].copy_from_slice(&key_buf[..key_buf.len().min(CT_BUF_LEN)]);
    let eq: bool = padded_token.ct_eq(&padded_key).into();
    // A truncated input can never be equal to the stored key: if the key
    // itself is longer than the buffer, no legitimate token fits either, so
    // every comparison must fail. Force mismatch when either side truncated.
    let ok = eq && !token_trunc && !key_trunc;
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
