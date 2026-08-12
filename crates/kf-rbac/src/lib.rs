//! kf-rbac — RBAC, timing-safe API-key auth, and OIDC JWT/JWKS verification.
//! Rust port of `@kirkforge/core-rbac` (425 LOC index.ts + 161 LOC jwt-verify.ts).

pub mod api_key;
pub mod error;
pub mod jwt;
pub mod rbac;

pub use api_key::actor_from_api_key;
pub use error::{AuthError, AuthErrorCode};
pub use jwt::{
    actor_from_jwt, clear_jwks_cache, validate_jwt_claims, verify_jwt, Aud, JwtClaims, OidcConfig,
    VerifyJwtOptions, ALLOWED_ALGORITHMS,
};
pub use rbac::{
    authorize, authorize_tenant, has_permission, resolve_role, role_permissions, Actor,
    AuthAuditHook, AuthDecision, AuthMethod, GroupRoleMapping, Permission, Role,
    DEFAULT_GROUP_ROLE_MAPPING,
};
