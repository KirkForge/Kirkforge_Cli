//! RBAC model: roles, permissions, actors, authorization, role resolution.
//! Pure map-lookup logic. Port of `@kirkforge/core-rbac/src/index.ts` (RBAC parts).

use crate::error::{AuthError, AuthErrorCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

// ── Roles ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Operator,
    Developer,
    Viewer,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Developer => "developer",
            Self::Viewer => "viewer",
        }
    }
}

/// Parse a role string. Unknown values are errors; callers fall back to
/// `Role::Viewer`. This is the parse boundary where the TS deny-by-default for
/// unknown roles is enforced (the enum makes invalid roles unconstructable).
impl std::str::FromStr for Role {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(Self::Admin),
            "operator" => Ok(Self::Operator),
            "developer" => Ok(Self::Developer),
            "viewer" => Ok(Self::Viewer),
            _ => Err(()),
        }
    }
}

// ── Permissions ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    // Admin
    AdminConfig,
    AdminPolicy,
    AdminTenant,
    AdminKeys,
    AdminAuditExport,
    // Operator
    OperatorHealth,
    OperatorRestart,
    OperatorViewAudit,
    // Developer
    DevVerify,
    DevCorrect,
    DevObserve,
    DevMemoryRead,
    DevMemoryWrite,
    // Viewer
    ViewerStatus,
    ViewerResults,
    ViewerMetrics,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AdminConfig => "admin:config",
            Self::AdminPolicy => "admin:policy",
            Self::AdminTenant => "admin:tenant",
            Self::AdminKeys => "admin:keys",
            Self::AdminAuditExport => "admin:audit_export",
            Self::OperatorHealth => "operator:health",
            Self::OperatorRestart => "operator:restart",
            Self::OperatorViewAudit => "operator:view_audit",
            Self::DevVerify => "dev:verify",
            Self::DevCorrect => "dev:correct",
            Self::DevObserve => "dev:observe",
            Self::DevMemoryRead => "dev:memory_read",
            Self::DevMemoryWrite => "dev:memory_write",
            Self::ViewerStatus => "viewer:status",
            Self::ViewerResults => "viewer:results",
            Self::ViewerMetrics => "viewer:metrics",
        }
    }
}

// ── Role → Permission mapping (deny-by-default: only listed perms are granted) ─

const ADMIN_PERMS: &[Permission] = &[
    Permission::AdminConfig,
    Permission::AdminPolicy,
    Permission::AdminTenant,
    Permission::AdminKeys,
    Permission::AdminAuditExport,
    Permission::OperatorHealth,
    Permission::OperatorRestart,
    Permission::OperatorViewAudit,
    Permission::DevVerify,
    Permission::DevCorrect,
    Permission::DevObserve,
    Permission::DevMemoryRead,
    Permission::DevMemoryWrite,
    Permission::ViewerStatus,
    Permission::ViewerResults,
    Permission::ViewerMetrics,
];

const OPERATOR_PERMS: &[Permission] = &[
    Permission::OperatorHealth,
    Permission::OperatorRestart,
    Permission::OperatorViewAudit,
    Permission::ViewerStatus,
    Permission::ViewerResults,
    Permission::ViewerMetrics,
];

const DEVELOPER_PERMS: &[Permission] = &[
    Permission::DevVerify,
    Permission::DevCorrect,
    Permission::DevObserve,
    Permission::DevMemoryRead,
    Permission::DevMemoryWrite,
    Permission::ViewerStatus,
    Permission::ViewerResults,
    Permission::ViewerMetrics,
];

const VIEWER_PERMS: &[Permission] = &[
    Permission::ViewerStatus,
    Permission::ViewerResults,
    Permission::ViewerMetrics,
];

/// Static permission set for a role. Deny-by-default is structural: a valid
/// `Role` always resolves to its slice, so unknown roles cannot grant anything.
pub fn role_permissions(role: Role) -> &'static [Permission] {
    match role {
        Role::Admin => ADMIN_PERMS,
        Role::Operator => OPERATOR_PERMS,
        Role::Developer => DEVELOPER_PERMS,
        Role::Viewer => VIEWER_PERMS,
    }
}

// ── Actor context ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Oidc,
    ApiKey,
    Internal,
}

impl AuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Oidc => "oidc",
            Self::ApiKey => "api_key",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    pub id: String,
    pub role: Role,
    pub tenant_id: String,
    pub auth_method: AuthMethod,
    /// ISO-8601 timestamp at which the actor's credentials were verified.
    pub verified_at: String,
}

// ── Auth audit hook ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AuthDecision {
    pub granted: bool,
    pub actor_id: String,
    pub role: Role,
    pub permission: Permission,
    pub target_tenant_id: Option<String>,
    pub actor_tenant_id: String,
    pub reason: String,
    pub timestamp: String,
}

/// Audit hook callback. `authorize`/`authorize_tenant` invoke it with each decision.
/// No `Send`/`Sync` bound: the callbacks fire synchronously within the (single-threaded)
/// authorization call, so thread-safety is the caller's concern, not the type's.
pub type AuthAuditHook = dyn Fn(&AuthDecision);

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ── Authorization ───────────────────────────────────────────────────────────

/// Check whether an actor has a specific permission. Deny-by-default.
pub fn has_permission(actor: &Actor, permission: Permission) -> bool {
    role_permissions(actor.role).contains(&permission)
}

/// Authorize an actor for a permission. `Ok(())` if granted, `Err(AuthError)` if
/// denied. Optionally records the decision via `audit_hook`.
pub fn authorize(
    actor: &Actor,
    permission: Permission,
    audit_hook: Option<&AuthAuditHook>,
) -> Result<(), AuthError> {
    if has_permission(actor, permission) {
        if let Some(hook) = audit_hook {
            hook(&AuthDecision {
                granted: true,
                actor_id: actor.id.clone(),
                role: actor.role,
                permission,
                target_tenant_id: None,
                actor_tenant_id: actor.tenant_id.clone(),
                reason: String::new(),
                timestamp: now_iso(),
            });
        }
        return Ok(());
    }
    let reason = format!(
        "Actor \"{}\" (role={}) does not have permission \"{}\"",
        actor.id,
        actor.role.as_str(),
        permission.as_str()
    );
    if let Some(hook) = audit_hook {
        hook(&AuthDecision {
            granted: false,
            actor_id: actor.id.clone(),
            role: actor.role,
            permission,
            target_tenant_id: None,
            actor_tenant_id: actor.tenant_id.clone(),
            reason: reason.clone(),
            timestamp: now_iso(),
        });
    }
    Err(AuthError::new(
        AuthErrorCode::Forbidden,
        reason,
        serde_json::json!({
            "actorId": actor.id,
            "role": actor.role.as_str(),
            "permission": permission.as_str(),
        }),
    ))
}

/// Authorize an actor for a permission scoped to a tenant. Cross-tenant access
/// is denied unless the actor is an admin. Inlined (not delegating to `authorize`)
/// to keep the audit-hook wiring simple — observable behavior matches the TS port.
pub fn authorize_tenant(
    actor: &Actor,
    permission: Permission,
    target_tenant_id: &str,
    audit_hook: Option<&AuthAuditHook>,
) -> Result<(), AuthError> {
    // Cross-tenant check first (before the permission check).
    if actor.role != Role::Admin && actor.tenant_id != target_tenant_id {
        let reason = format!(
            "Actor \"{}\" (role={}, tenant={}) cannot access tenant \"{}\"",
            actor.id,
            actor.role.as_str(),
            actor.tenant_id,
            target_tenant_id
        );
        if let Some(hook) = audit_hook {
            hook(&AuthDecision {
                granted: false,
                actor_id: actor.id.clone(),
                role: actor.role,
                permission,
                target_tenant_id: Some(target_tenant_id.to_string()),
                actor_tenant_id: actor.tenant_id.clone(),
                reason: reason.clone(),
                timestamp: now_iso(),
            });
        }
        return Err(AuthError::new(
            AuthErrorCode::Forbidden,
            reason,
            serde_json::json!({
                "actorId": actor.id,
                "role": actor.role.as_str(),
                "tenantId": actor.tenant_id,
                "targetTenantId": target_tenant_id,
                "permission": permission.as_str(),
            }),
        ));
    }
    // Same-tenant (or admin): permission check, emitting the decision with the
    // target tenant filled in.
    if has_permission(actor, permission) {
        if let Some(hook) = audit_hook {
            hook(&AuthDecision {
                granted: true,
                actor_id: actor.id.clone(),
                role: actor.role,
                permission,
                target_tenant_id: Some(target_tenant_id.to_string()),
                actor_tenant_id: actor.tenant_id.clone(),
                reason: String::new(),
                timestamp: now_iso(),
            });
        }
        return Ok(());
    }
    let reason = format!(
        "Actor \"{}\" (role={}) does not have permission \"{}\"",
        actor.id,
        actor.role.as_str(),
        permission.as_str()
    );
    if let Some(hook) = audit_hook {
        hook(&AuthDecision {
            granted: false,
            actor_id: actor.id.clone(),
            role: actor.role,
            permission,
            target_tenant_id: Some(target_tenant_id.to_string()),
            actor_tenant_id: actor.tenant_id.clone(),
            reason: reason.clone(),
            timestamp: now_iso(),
        });
    }
    Err(AuthError::new(
        AuthErrorCode::Forbidden,
        reason,
        serde_json::json!({
            "actorId": actor.id,
            "role": actor.role.as_str(),
            "permission": permission.as_str(),
        }),
    ))
}

// ── Role resolution from groups/claims ──────────────────────────────────────

/// Group name (or claim value) → role mapping.
#[derive(Debug, Clone, Default)]
pub struct GroupRoleMapping(pub HashMap<String, Role>);

pub static DEFAULT_GROUP_ROLE_MAPPING: LazyLock<GroupRoleMapping> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("kirkforge-admins".to_string(), Role::Admin);
    m.insert("kirkforge-operators".to_string(), Role::Operator);
    m.insert("kirkforge-developers".to_string(), Role::Developer);
    m.insert("kirkforge-viewers".to_string(), Role::Viewer);
    m.insert("admins".to_string(), Role::Admin);
    m.insert("operators".to_string(), Role::Operator);
    m.insert("developers".to_string(), Role::Developer);
    m.insert("viewers".to_string(), Role::Viewer);
    GroupRoleMapping(m)
});

/// Map a set of group names or OIDC claims to a role. Highest-privilege role
/// wins (admin > operator > developer > viewer). Falls back to viewer.
pub fn resolve_role(groups: &[String], mapping: Option<&GroupRoleMapping>) -> Role {
    let default = &DEFAULT_GROUP_ROLE_MAPPING;
    let m = mapping.unwrap_or(default);
    // priority order: highest privilege first.
    let priority = [Role::Admin, Role::Operator, Role::Developer, Role::Viewer];
    for &p in &priority {
        for group in groups {
            if let Some(&r) = m.0.get(group) {
                if r == p {
                    return p;
                }
            }
        }
    }
    Role::Viewer
}
