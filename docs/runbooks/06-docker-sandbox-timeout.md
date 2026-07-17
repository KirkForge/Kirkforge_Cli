# Runbook: Docker Sandbox Timeout

## Symptom

- A verifier or tool subprocess is killed after the configured timeout
- Verification result for the slot is `fail` with a timeout error class
- The host runner logs a subprocess exit from `timeout`/SIGTERM

## Diagnosis

1. Check the configured per-tool timeout in the host runner config (the field that feeds the verifier spawn timeout).
2. Reproduce by running the failing tool directly inside the sandbox image:
   ```bash
   docker run --rm --network=none -v "$PWD:/work" ghcr.io/kirkforge/kirkforge-sandbox:latest \
     /usr/bin/timeout 60 <tool-command>
   ```
3. If the tool completes in <60s outside the sandbox but times out inside, the sandbox CPU/network restriction is the cause.

## Resolution

- Raise the per-tool timeout in the host runner config (operator decision — large repos need more).
- If the tool is genuinely CPU-bound, run it on the host under `read-only` trust instead of the Docker sandbox (only for trusted, audited tools).
- If the tool hangs (not slow), file a bug against the tool; do not paper over it with a larger timeout.

## Prevention

- The Docker sandbox runner enforces `--network=none` (see `SECURITY.md`); tools that need network must declare `network` trust and run on the host runner.
- `docs/sla-definitions.md` defines P95 targets; tune the per-tool timeout so the SLO holds.