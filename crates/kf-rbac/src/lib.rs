//! kf-rbac — RBAC and timing-safe API-key auth.
//! Rust port of the RBAC half of `@kirkforge/core-rbac` (425 LOC index.ts).
//! The OIDC JWT/JWKS half (jwt-verify.ts port) was deleted in WO 47.3 —
//! dead code; the daemon authenticates via token + role file (WO 43.6).

pub mod api_key;
pub mod error;
pub mod rbac;

pub use api_key::actor_from_api_key;
pub use error::{AuthError, AuthErrorCode};
pub use rbac::{
    authorize, authorize_tenant, has_permission, resolve_role, role_permissions, Actor,
    AuthAuditHook, AuthDecision, AuthMethod, GroupRoleMapping, Permission, Role,
    DEFAULT_GROUP_ROLE_MAPPING,
};
