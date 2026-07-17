# Runbook: Quota Exceeded

## Symptom

- API/CLI call returns `quota_exceeded` or `429`-equivalent
- `core-enterprise` `recordUsage` logs show a tenant hitting their configured ceiling
- Host agent receives a `quota_exceeded` error envelope from the orchestrator

## Diagnosis

1. Identify the tenant and which quota dimension was hit (requests, tokens, verifications, etc.).
2. Inspect the persisted quota state (enterprise mode writes here on each `recordUsage`):
   ```bash
   cat "${QUOTA_PERSISTENCE_PATH:-.kirkforge/quotas.json}" | jq .
   ```
3. Compare current usage against the configured limit for that tenant in your enterprise policy config.

## Resolution

- **Legitimate growth**: raise the tenant's quota in the enterprise config (`packages/core-enterprise` policy); apply and reload policy.
- **Runaway loop**: a stuck correction loop can burn verifications. Stop the agent, fix the underlying task (see `02-circuit-breaker-trip.md`), then resume.
- **Misconfigured tool**: a tool spamming verifications per file should be rate-limited at the orchestrator, not by quota alone.

## Prevention

- Alert on tenants crossing 80% of any quota so growth is handled before exhaustion.
- Pair quota ceilings with the correction-loop circuit breaker (see `02-circuit-breaker-trip.md`) so a stuck loop cannot drain a tenant's entire quota.