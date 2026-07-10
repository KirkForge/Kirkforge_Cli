# @kirkforge/core-tenancy

Multi-tenant isolation layer. Scopes storage, events, and configuration to individual tenants.

## Key exports

- `TenantContext` — tenant-scoped context object
- `createTenantScope(tenantId)` — factory for tenant isolation
