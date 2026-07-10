# @kirkforge/plugin

Public API surface for the KirkForge deterministic verification, correction, and routing layer. This is the npm package host agents integrate with — Codex, Claude Code, OpenCode, LangChain-style systems, or any Node.js process that wants a verification gate and empirical routing memory.

## Install

```bash
npm install @kirkforge/plugin
```

Peer dependencies you'll likely need alongside:

```bash
npm install @kirkforge/memory-palace @kirkforge/correction-core @kirkforge/core-events
```

## What's in the box

Six functional groups, all exported from the package root:

| Group                          | Exports                                                                                              | What it does                                                                                            |
| ------------------------------ | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| **Verification**               | `verifyWorkspace`, `verifyWorkspaceWithEmitter`                                                       | Runs the deterministic verifier battery on a workspace. No model calls.                                 |
| **Correction**                 | `buildCorrectionPrompt`                                                                              | Builds a compact correction prompt from a `ReducedStatePacket`. No model calls.                         |
| **Memory**                     | `recordObservation`, `recallRoutingBias`                                                              | Writes task outcomes and reads routing bias for similar tasks. Memory store is provided by the caller. |
| **Tooling**                    | `doctor`                                                                                             | Probes external tools (`eslint`, `tsc`, `ruff`, `pyright`, `bandit`, `git`) and reports availability.   |
| **Authentication & tenancy**   | `createAuthMiddleware`, `AuthMiddleware`, `createTenantContext`, `createTenantAuditLogger`           | OIDC JWT / API key auth, RBAC, multi-tenant scoping, audit wiring.                                      |
| **Audit bridge**               | `createAuthAuditHook`                                                                                | Wires `core-rbac` decisions into the audit log.                                                         |

Stable / Beta / Experimental ratings are tracked in [`docs/STABILITY_MATRIX.md`](../../docs/STABILITY_MATRIX.md).

## Quick start

```ts
import { createPluginCore } from "@kirkforge/plugin";
import { MemoryStore, InMemoryAdapter } from "@kirkforge/memory-palace";

const memory = new MemoryStore(new InMemoryAdapter());
const plugin = createPluginCore({ memoryStore: memory });

// 1. Verify a workspace
const result = await plugin.verifyWorkspace({
  workspace: "/path/to/project",
  language: "typescript",
  description: "Add user authentication",
});

if (!result.ok) {
  console.error("verify failed:", result.error);
  return;
}

const packet = result.value;
console.log("Overall:", packet.overall); // "pass" | "warn" | "fail"

// 2. If the packet didn't pass, build a correction prompt
if (packet.overall !== "pass") {
  const prompt = plugin.buildCorrectionPrompt(packet, { language: "typescript" });
  // Send `prompt` to your worker model for a targeted retry
}

// 3. After the task resolves (whatever the model produced), record the outcome
await plugin.recordObservation({
  taskId: "auth-1",
  description: "Add user authentication",
  language: "typescript",
  mode: "hard-prompt",
  model: "gpt-4o-mini",
  outcome: "pass",
  durationMs: 12_400,
  verifierOverall: packet.overall,
});

// 4. Next time a similar task arrives, recall the routing bias
const bias = await plugin.recallRoutingBias("Add user authentication");
if (bias.ok && bias.value) {
  console.log("Prefer:", bias.value.prefer, "Avoid:", bias.value.avoid);
}

// 5. Check whether the local environment has the verification tools installed
const report = await plugin.doctor();
console.log("Available languages:", report.languages);
```

## Authentication & multi-tenancy

`@kirkforge/plugin` ships auth middleware and tenant context for production deployments. The verification pipeline does not require either; these are guardrails for when you wire the plugin into an HTTP service or a multi-tenant SaaS.

```ts
import {
  createAuthMiddleware,
  createTenantContext,
  createAuthAuditHook,
} from "@kirkforge/plugin";
import { AuditLogger, FileAuditSink } from "@kirkforge/core-events";
import { authorize } from "@kirkforge/core-rbac";

// 1. Set up an audit logger
const audit = new AuditLogger(new FileAuditSink({ filePath: "/var/log/kirkforge/audit.jsonl" }));

// 2. Create auth middleware (OIDC + API key fallback)
const auth = createAuthMiddleware({
  oidcConfig: {
    issuer: "https://auth.example.com",
    audience: "kirkforge-api",
    jwksUri: "https://auth.example.com/.well-known/jwks.json",
  },
  apiKey: process.env.KIRKFORGE_API_KEY, // optional
  auditLogger: audit,
  requireAuth: true, // flip on in production
});

// 3. Wire a tenant context for multi-tenant isolation
const ctx = await createTenantContext({
  tenantId: "tenant-acme",
  actorId: "user-123",
  auditSink: new FileAuditSink({ filePath: "/var/log/kirkforge/audit-acme.jsonl" }),
});

// 4. On each request, authenticate then check permission
const authResult = await auth.authenticate(request.headers.authorization);
if (!authResult.ok) return respond(401, authResult.error.message);

const permCheck = auth.checkPermission(authResult.value.actor, "dev:verify");
if (!permCheck.ok) return respond(403, permCheck.error.message);

// 5. Use the tenant-scoped memory store (no cross-tenant leakage)
await ctx.memoryStore.writeTaskObservation({ ... });

// 6. Bridge every auth decision to the audit log
const auditHook = createAuthAuditHook(audit, "tenant-acme");
authorize(actor, "dev:verify", auditHook);
```

The auth middleware accepts `Bearer <token>` headers where the token is either an OIDC JWT (verified against the configured JWKS) or a static API key (≥ 32 chars enforced). It never silently downgrades a failed JWT to API-key auth in enterprise mode.

## MCP server integration

The [`@kirkforge/mcp`](../../apps/mcp) server exposes the same five core operations (`verifyWorkspace`, `doctor`, `buildCorrectionPrompt`, `recordObservation`, `recallRoutingBias`) via Model Context Protocol. Drop it into any MCP host:

```json
{
  "mcpServers": {
    "kirkforge": {
      "command": "npx",
      "args": ["@kirkforge/mcp"]
    }
  }
}
```

Tools exposed: `kirkforge_verify_workspace`, `kirkforge_doctor`, `kirkforge_record_observation`, `kirkforge_recall_routing_bias`, `kirkforge_build_correction_prompt`.

## CLI shell adapter

If your host cannot import a Node module, the `kirkforge` CLI (built on `@kirkforge/plugin`) accepts the same inputs over stdout. See [docs/PLUGIN_CLI_CONTRACT.md](../../docs/PLUGIN_CLI_CONTRACT.md) for the full contract.

```bash
kirkforge verify-workspace --workspace /path/to/project
kirkforge prompt --packet result.json
kirkforge observe --memory mem.json --task-id t1 --description "fix auth" \
  --language typescript --mode hard-prompt --model gpt-4o-mini \
  --outcome pass --duration-ms 5000
kirkforge recall --memory mem.json --description "fix auth"
```

## API reference

### `verifyWorkspace(input) -> Promise<Result<ReducedStatePacket, Error>>`

Runs the deterministic verifier battery in parallel. The reducer folds all signals into one `ReducedStatePacket` with `overall: "pass" | "warn" | "fail"`. **Never throws on verifier error** — individual emitter errors become `status: "error"` in their slot, which the reducer treats as fail (see fail-closed contract in `docs/STABILITY_MATRIX.md`).

| Field       | Type     | Required | Description                                          |
| ----------- | -------- | -------- | ---------------------------------------------------- |
| `workspace` | `string` | yes      | Absolute path to the project root                    |
| `files`     | `string[]` | no     | Subset of files to verify; defaults to all changed   |
| `language`  | `string` | no       | `typescript` \| `javascript` \| `python` \| `shell` \| `cpp` \| `c` \| `rust` \| `go` \| `sql` \| `text` |
| `description` | `string` | no     | Free-text hint for `detectTaskProfile`               |
| `taskId`    | `string` | no       | Task identifier for event correlation (auto-generated if omitted) |

### `buildCorrectionPrompt(packet, context?) -> string`

Given a non-`pass` packet, returns a compact prompt describing exactly what failed and where. No model calls. Pass `context.language` to get language-specific tool names in the prompt (`ruff` for Python, `tsc` for TypeScript, etc.).

### `recordObservation(observation) -> Promise<Result<void, Error>>`

Records a single task outcome for future routing. **The host must provide the actual task outcome** — never derive it from `packet.overall`. Recording verifier-pass as task-pass poisons routing memory with false positives.

### `recallRoutingBias(description, workerModel?) -> Promise<Result<RoutingBias | null, Error>>`

Returns a `RoutingBias` (prefer/avoid lists, confidence, evidence count) for tasks similar to `description`, or `null` if memory is empty. Cosine similarity is computed locally over FNV-1a fingerprint vectors.

### `doctor() -> Promise<ToolCapabilityReport>`

Probes external tools via `execFile` and reports availability, version, and source (internal vs. external). No state changes. Use it during install validation and as a health check.

### `createAuthMiddleware(config) -> AuthMiddleware`

Returns an `AuthMiddleware` instance. See the example above for full wiring.

| Field                       | Type                | Description                                                  |
| --------------------------- | ------------------- | ------------------------------------------------------------ |
| `oidcConfig`                | `OidcConfig`        | JWKS URI, issuer, audience. Verified on every request.      |
| `apiKey`                    | `string`            | Static bearer-token fallback. ≥ 32 chars in enterprise mode. |
| `groupRoleMapping`          | `GroupRoleMapping`  | OIDC group claims → role mapping.                            |
| `auditLogger`               | `AuditLogger`       | Auth events get recorded here.                               |
| `requireAuth`               | `boolean`           | `true` in enterprise mode, `false` in dev.                   |
| `allowApiKeyFallbackWithOidc` | `boolean`         | `false` in enterprise mode (no silent JWT → API-key downgrade). |

### `createTenantContext(config) -> Promise<TenantContext>`

Returns a fully wired `TenantContext` (tenant-scoped memory store + audit logger + optional quota manager). See example above.

## Stability

The verification surface (`verifyWorkspace`, `buildCorrectionPrompt`, `recordObservation`, `recallRoutingBias`, `doctor`) is **Stable** — the `Result<T, E>` envelope, `ReducedStatePacket` shape, and `ToolCapabilityReport` shape are not expected to change in backward-incompatible ways without a migration.

The auth/tenant surface (`createAuthMiddleware`, `createTenantContext`, `createAuthAuditHook`) is **Beta** — works for the common case but the policy/role model may shift.

The `MemoryStore` you pass in is **your** state. `InMemoryAdapter` is fine for tests; `FileAdapter` and `SqliteAdapter` from `@kirkforge/memory-palace` are the production options. Cross-process atomicity on `FileAdapter` is best-effort; use `SqliteAdapter` for concurrent automation.

## Caveats

- **Verifier pass ≠ task pass.** `verifyWorkspace` returning `pass` does not mean the task is done. Task validators, provided by the host, are the authority on task completion. The plugin never decides that.
- **No model calls in verification or correction paths.** All five core commands are deterministic. This is a load-bearing invariant.
- **The plugin never stores or refreshes API keys.** Model configuration is the host's responsibility. The plugin may accept a model config for token estimation but does not own provider auth long-term.

## License

Apache-2.0
