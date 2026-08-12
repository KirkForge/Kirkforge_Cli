//! Port of `@kirkforge/core-rbac/tests/index.test.ts` (46 RBAC tests).
//! `unknown role` tests (TS #8, #42) become `Role::from_str` rejection tests:
//! the TS deny-by-default for unknown roles is enforced at the Rust parse
//! boundary because the `Role` enum makes invalid roles unconstructable.

use kf_rbac::{
    actor_from_api_key, actor_from_jwt, authorize, authorize_tenant, has_permission, resolve_role,
    role_permissions, Aud, AuthDecision, AuthErrorCode, GroupRoleMapping, JwtClaims, OidcConfig,
    Permission, Role,
};
use std::cell::RefCell;
use std::rc::Rc;

fn actor(id: &str, role: Role) -> kf_rbac::Actor {
    kf_rbac::Actor {
        id: id.to_string(),
        role,
        tenant_id: "t1".to_string(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn claims(sub: &str, exp: i64, iat: i64) -> JwtClaims {
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

// ── hasPermission ───────────────────────────────────────────────────────────

#[test]
fn admin_has_all_admin_permissions() {
    let a = actor("admin1", Role::Admin);
    assert!(has_permission(&a, Permission::AdminConfig));
    assert!(has_permission(&a, Permission::AdminPolicy));
    assert!(has_permission(&a, Permission::AdminTenant));
    assert!(has_permission(&a, Permission::AdminKeys));
    assert!(has_permission(&a, Permission::AdminAuditExport));
}

#[test]
fn admin_inherits_operator_developer_viewer_permissions() {
    let a = actor("admin1", Role::Admin);
    assert!(has_permission(&a, Permission::OperatorHealth));
    assert!(has_permission(&a, Permission::DevVerify));
    assert!(has_permission(&a, Permission::ViewerStatus));
}

#[test]
fn operator_cannot_access_admin_or_developer_permissions() {
    let a = actor("op1", Role::Operator);
    assert!(!has_permission(&a, Permission::AdminConfig));
    assert!(!has_permission(&a, Permission::DevVerify));
}

#[test]
fn operator_has_health_restart_audit_and_viewer_permissions() {
    let a = actor("op1", Role::Operator);
    assert!(has_permission(&a, Permission::OperatorHealth));
    assert!(has_permission(&a, Permission::OperatorRestart));
    assert!(has_permission(&a, Permission::OperatorViewAudit));
    assert!(has_permission(&a, Permission::ViewerStatus));
}

#[test]
fn developer_can_verify_correct_observe_and_access_memory() {
    let a = actor("dev1", Role::Developer);
    assert!(has_permission(&a, Permission::DevVerify));
    assert!(has_permission(&a, Permission::DevCorrect));
    assert!(has_permission(&a, Permission::DevObserve));
    assert!(has_permission(&a, Permission::DevMemoryRead));
    assert!(has_permission(&a, Permission::DevMemoryWrite));
}

#[test]
fn developer_cannot_access_admin_or_operator_permissions() {
    let a = actor("dev1", Role::Developer);
    assert!(!has_permission(&a, Permission::AdminConfig));
    assert!(!has_permission(&a, Permission::OperatorRestart));
}

#[test]
fn viewer_can_only_view_status_results_and_metrics() {
    let a = actor("view1", Role::Viewer);
    assert!(has_permission(&a, Permission::ViewerStatus));
    assert!(has_permission(&a, Permission::ViewerResults));
    assert!(has_permission(&a, Permission::ViewerMetrics));
    assert!(!has_permission(&a, Permission::DevVerify));
    assert!(!has_permission(&a, Permission::AdminConfig));
}

#[test]
fn unknown_role_denies_everything() {
    // Adapted from TS: with a typed enum an invalid role cannot be constructed.
    // The security property (unknown role strings grant nothing) is enforced at
    // the parse boundary — Role::from_str returns Err for unknown values.
    assert!("unknown".parse::<Role>().is_err());
    assert_eq!(role_permissions(Role::Viewer).len(), 3);
}

// ── authorize ───────────────────────────────────────────────────────────────

#[test]
fn authorize_returns_ok_for_granted_permissions() {
    let v = actor("v1", Role::Viewer);
    assert!(authorize(&v, Permission::ViewerStatus, None).is_ok());
}

#[test]
fn authorize_returns_err_for_denied_permissions() {
    let v = actor("v1", Role::Viewer);
    let err = authorize(&v, Permission::AdminConfig, None).unwrap_err();
    assert_eq!(err.code, AuthErrorCode::Forbidden);
}

// ── authorizeTenant ─────────────────────────────────────────────────────────

#[test]
fn authorize_tenant_allows_same_tenant_access() {
    let dev = kf_rbac::Actor {
        id: "dev1".into(),
        role: Role::Developer,
        tenant_id: "t1".into(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".into(),
    };
    assert!(authorize_tenant(&dev, Permission::DevVerify, "t1", None).is_ok());
}

#[test]
fn authorize_tenant_denies_cross_tenant_for_non_admin() {
    let dev = kf_rbac::Actor {
        id: "dev1".into(),
        role: Role::Developer,
        tenant_id: "t1".into(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let err = authorize_tenant(&dev, Permission::DevVerify, "t2", None).unwrap_err();
    assert_eq!(err.code, AuthErrorCode::Forbidden);
}

#[test]
fn authorize_tenant_allows_cross_tenant_for_admin() {
    let admin = kf_rbac::Actor {
        id: "admin1".into(),
        role: Role::Admin,
        tenant_id: "t0".into(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".into(),
    };
    assert!(authorize_tenant(&admin, Permission::AdminConfig, "t1", None).is_ok());
}

// ── resolveRole ─────────────────────────────────────────────────────────────

#[test]
fn resolve_role_known_group_to_role() {
    let m = GroupRoleMapping(
        [
            ("admins".to_string(), Role::Admin),
            ("devs".to_string(), Role::Developer),
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(resolve_role(&["admins".to_string()], Some(&m)), Role::Admin);
}

#[test]
fn resolve_role_defaults_to_viewer_for_unknown_groups() {
    let m = GroupRoleMapping([("admins".to_string(), Role::Admin)].into_iter().collect());
    assert_eq!(
        resolve_role(&["unknown-group".to_string()], Some(&m)),
        Role::Viewer
    );
}

#[test]
fn resolve_role_prioritizes_admin_over_other_roles() {
    let m = GroupRoleMapping(
        [
            ("admins".to_string(), Role::Admin),
            ("devs".to_string(), Role::Developer),
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(
        resolve_role(&["devs".to_string(), "admins".to_string()], Some(&m)),
        Role::Admin
    );
}

#[test]
fn resolve_role_defaults_to_viewer_without_mapping() {
    assert_eq!(resolve_role(&["any-group".to_string()], None), Role::Viewer);
}

// ── validateJwtClaims ───────────────────────────────────────────────────────

fn config() -> OidcConfig {
    OidcConfig {
        issuer: "https://auth.example.com".to_string(),
        audience: "kirkforge".to_string(),
        jwks_uri: None,
        clock_skew_sec: None,
    }
}

#[test]
fn validate_claims_accepts_valid_claims() {
    let now = 1_700_000_000_000_i64;
    let c = claims("user1", now / 1000 + 3600, now / 1000 - 60);
    assert!(kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).is_ok());
}

#[test]
fn validate_claims_rejects_wrong_issuer() {
    let now = 1_700_000_000_000_i64;
    let mut c = claims("user1", now / 1000 + 3600, now / 1000 - 60);
    c.iss = "https://evil.com".to_string();
    let err = kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).unwrap_err();
    assert_eq!(err.code, AuthErrorCode::InvalidToken);
}

#[test]
fn validate_claims_rejects_wrong_audience() {
    let now = 1_700_000_000_000_i64;
    let mut c = claims("user1", now / 1000 + 3600, now / 1000 - 60);
    c.aud = Aud::One("wrong-aud".to_string());
    assert!(kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).is_err());
}

#[test]
fn validate_claims_rejects_expired_token() {
    let now = 1_700_000_000_000_i64;
    let c = claims("user1", now / 1000 - 100, now / 1000 - 3600);
    let err = kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).unwrap_err();
    assert!(err.message.contains("expired"));
}

#[test]
fn validate_claims_rejects_future_issued_at() {
    let now = 1_700_000_000_000_i64;
    let c = claims("user1", now / 1000 + 7200, now / 1000 + 3600);
    let err = kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).unwrap_err();
    assert!(err.message.contains("future"));
}

#[test]
fn validate_claims_accepts_audience_as_array() {
    let now = 1_700_000_000_000_i64;
    let mut c = claims("user1", now / 1000 + 3600, now / 1000 - 60);
    c.aud = Aud::Many(vec!["kirkforge".to_string(), "other".to_string()]);
    assert!(kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).is_ok());
}

#[test]
fn validate_claims_allows_clock_skew_tolerance() {
    let now = 1_700_000_000_000_i64;
    let cfg = OidcConfig {
        clock_skew_sec: Some(120),
        ..config()
    };
    let c = claims("user1", now / 1000 - 60, now / 1000 - 3600);
    assert!(kf_rbac::validate_jwt_claims(&c, &cfg, Some(now)).is_ok());
}

// ── actorFromJwt ────────────────────────────────────────────────────────────

#[test]
fn actor_from_jwt_extracts_actor_with_group_mapping() {
    let m = GroupRoleMapping(
        [
            ("admins".to_string(), Role::Admin),
            ("devs".to_string(), Role::Developer),
        ]
        .into_iter()
        .collect(),
    );
    let mut c = claims("user1", 9999999999, 1000);
    c.groups = Some(vec!["devs".to_string()]);
    let a = actor_from_jwt(&c, &config(), Some(&m)).unwrap();
    assert_eq!(a.role, Role::Developer);
    assert_eq!(a.auth_method, kf_rbac::AuthMethod::Oidc);
}

#[test]
fn actor_from_jwt_defaults_to_viewer_without_groups() {
    let c = claims("user1", 9999999999, 1000);
    let a = actor_from_jwt(&c, &config(), None).unwrap();
    assert_eq!(a.role, Role::Viewer);
}

// ── actorFromApiKey ─────────────────────────────────────────────────────────

#[test]
fn api_key_accepts_matching_key() {
    let a = actor_from_api_key(
        "abcdef1234567890abcdef1234567890",
        "abcdef1234567890abcdef1234567890",
        Role::Operator,
        "",
    )
    .unwrap();
    assert_eq!(a.auth_method, kf_rbac::AuthMethod::ApiKey);
    assert_eq!(a.role, Role::Operator);
}

#[test]
fn api_key_rejects_mismatched_key() {
    let err = actor_from_api_key(
        "abcdef1234567890abcdef1234567890",
        "00000000000000000000000000000000",
        Role::Operator,
        "",
    )
    .unwrap_err();
    assert_eq!(err.code, AuthErrorCode::InvalidToken);
}

#[test]
fn api_key_rejects_empty_token() {
    let err = actor_from_api_key("", "some-key", Role::Operator, "").unwrap_err();
    assert_eq!(err.code, AuthErrorCode::Unauthorized);
}

#[test]
fn api_key_uses_provided_role_and_tenant() {
    let key = "abcdef1234567890abcdef1234567890";
    let a = actor_from_api_key(key, key, Role::Viewer, "t1").unwrap();
    assert_eq!(a.role, Role::Viewer);
    assert_eq!(a.tenant_id, "t1");
}

// ── authorize with audit hook ───────────────────────────────────────────────

fn capture() -> (Rc<RefCell<Vec<AuthDecision>>>, impl Fn(&AuthDecision)) {
    let store: Rc<RefCell<Vec<AuthDecision>>> = Rc::new(RefCell::new(vec![]));
    let s = Rc::clone(&store);
    // ponytail: Rc share (hook is `dyn Fn`, no Send/Sync needed) so the test
    // and the closure observe the same Vec.
    let closure = move |d: &AuthDecision| s.borrow_mut().push(d.clone());
    (store, closure)
}

#[test]
fn authorize_calls_audit_hook_on_grant() {
    let v = actor("v1", Role::Viewer);
    let (store, hook) = capture();
    let result = authorize(&v, Permission::ViewerStatus, Some(&hook));
    assert!(result.is_ok());
    let got = store.borrow();
    assert_eq!(got.len(), 1);
    assert!(got[0].granted);
    assert_eq!(got[0].permission, Permission::ViewerStatus);
    assert_eq!(got[0].actor_id, "v1");
    assert!(got[0].reason.is_empty());
}

#[test]
fn authorize_calls_audit_hook_on_deny() {
    let v = actor("v1", Role::Viewer);
    let (store, hook) = capture();
    let result = authorize(&v, Permission::AdminConfig, Some(&hook));
    assert!(result.is_err());
    let got = store.borrow();
    assert_eq!(got.len(), 1);
    assert!(!got[0].granted);
    assert_eq!(got[0].permission, Permission::AdminConfig);
    assert!(got[0].reason.contains("does not have permission"));
}

#[test]
fn authorize_works_without_audit_hook() {
    let v = actor("v1", Role::Viewer);
    assert!(authorize(&v, Permission::ViewerStatus, None).is_ok());
}

#[test]
fn authorize_tenant_calls_audit_hook_on_deny() {
    let dev = kf_rbac::Actor {
        id: "dev1".into(),
        role: Role::Developer,
        tenant_id: "t1".into(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let (store, hook) = capture();
    let result = authorize_tenant(&dev, Permission::DevVerify, "t2", Some(&hook));
    assert!(result.is_err());
    let got = store.borrow();
    assert_eq!(got.len(), 1);
    assert!(!got[0].granted);
    assert_eq!(got[0].target_tenant_id.as_deref(), Some("t2"));
    assert!(got[0].reason.contains("cannot access tenant"));
}

#[test]
fn authorize_tenant_calls_audit_hook_on_grant() {
    let dev = kf_rbac::Actor {
        id: "dev1".into(),
        role: Role::Developer,
        tenant_id: "t1".into(),
        auth_method: kf_rbac::AuthMethod::Oidc,
        verified_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let (store, hook) = capture();
    let result = authorize_tenant(&dev, Permission::DevVerify, "t1", Some(&hook));
    assert!(result.is_ok());
    let got = store.borrow();
    assert_eq!(got.len(), 1);
    assert!(got[0].granted);
    assert_eq!(got[0].target_tenant_id.as_deref(), Some("t1"));
}

// ── negative auth scenarios ─────────────────────────────────────────────────

#[test]
fn negative_accepts_jwt_with_missing_required_claims() {
    // TS: empty sub is technically valid; claims validation accepts it. Same here.
    let c = claims("", 9999999999, 1000);
    assert!(kf_rbac::validate_jwt_claims(&c, &config(), None).is_ok());
}

#[test]
fn negative_rejects_jwt_with_malformed_issuer_trailing_slash() {
    let now = 1_700_000_000_000_i64;
    let mut c = claims("user1", 9999999999, 1000);
    c.iss = "https://auth.example.com/".to_string();
    let err = kf_rbac::validate_jwt_claims(&c, &config(), Some(now)).unwrap_err();
    assert!(err.message.contains("issuer mismatch"));
}

#[test]
fn negative_rejects_jwt_with_empty_audience() {
    let mut c = claims("user1", 9999999999, 1000);
    c.aud = Aud::One(String::new());
    assert!(kf_rbac::validate_jwt_claims(&c, &config(), None).is_err());
}

#[test]
fn negative_rejects_deeply_expired_jwt() {
    let now = 1_700_000_000_000_i64;
    let cfg = OidcConfig {
        clock_skew_sec: Some(30),
        ..config()
    };
    let c = claims("user1", now / 1000 - 86400, now / 1000 - 100000);
    let err = kf_rbac::validate_jwt_claims(&c, &cfg, Some(now)).unwrap_err();
    assert!(err.message.contains("expired"));
}

#[test]
fn negative_rejects_api_key_with_length_mismatch() {
    let err =
        actor_from_api_key("short", "much-longer-key-value-here", Role::Operator, "").unwrap_err();
    assert_eq!(err.code, AuthErrorCode::InvalidToken);
}

#[test]
fn negative_rejects_null_ish_api_key() {
    let err = actor_from_api_key("", "", Role::Operator, "").unwrap_err();
    assert_eq!(err.code, AuthErrorCode::Unauthorized);
}

#[test]
fn negative_unknown_role_cannot_access_any_permission() {
    // Adapted from TS: invalid role strings do not resolve to a Role variant.
    assert!("superadmin".parse::<Role>().is_err());
    assert!("ghost".parse::<Role>().is_err());
}

// ── timing-safe padding ─────────────────────────────────────────────────────

#[test]
fn timing_safe_rejects_token_shorter_than_key() {
    let err =
        actor_from_api_key("short", "much-longer-api-key-value", Role::Operator, "").unwrap_err();
    assert_eq!(err.code, AuthErrorCode::InvalidToken);
}

#[test]
fn timing_safe_rejects_token_longer_than_key() {
    let err = actor_from_api_key(
        "much-longer-token-value-than-key",
        "shortkey",
        Role::Operator,
        "",
    )
    .unwrap_err();
    assert_eq!(err.code, AuthErrorCode::InvalidToken);
}

#[test]
fn timing_safe_accepts_matching_token_and_key() {
    let secret = "my-api-key-12345";
    let a = actor_from_api_key(secret, secret, Role::Operator, "").unwrap();
    assert_eq!(a.role, Role::Operator);
    assert_eq!(a.auth_method, kf_rbac::AuthMethod::ApiKey);
}

#[test]
fn timing_safe_accepts_matching_with_role_and_tenant() {
    let secret = "top-secret-key";
    let a = actor_from_api_key(secret, secret, Role::Admin, "tenant-1").unwrap();
    assert_eq!(a.role, Role::Admin);
    assert_eq!(a.tenant_id, "tenant-1");
}
