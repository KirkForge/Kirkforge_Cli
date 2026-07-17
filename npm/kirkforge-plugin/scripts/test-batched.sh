#!/usr/bin/env bash
# Test runner that batches test files into groups for parallel execution.
# Each group runs in a separate vitest process with a timeout to prevent hangs.
set -euo pipefail

V="node node_modules/.bin/vitest"
R="--reporter=dot --testTimeout=30000 --hookTimeout=15000"
L="/tmp/kirkforge-test.log"
FAIL=0
# Per-batch timeout in seconds (prevents hung processes from blocking CI)
BATCH_TIMEOUT=180

# Run a vitest batch with a timeout. Args: <batch_name> <test_files...>
run_batch() {
  local name="$1"; shift
  printf "  %-40s " "$name"
  if timeout "${BATCH_TIMEOUT}s" $V run "$@" $R > $L 2>&1; then
    echo "PASS"
  else
    local rc=$?
    if [ $rc -eq 124 ]; then
      echo "TIMEOUT"
      echo "    (timed out after ${BATCH_TIMEOUT}s, see $L)"
    else
      echo "FAIL"
      tail -10 $L
    fi
    FAIL=1
  fi
}

echo "=== Test Suite ==="

run_batch "core-types+logging+tenancy" \
  packages/core-types/tests/result.test.ts \
  packages/core-logging/tests/index.test.ts \
  packages/core-tenancy/tests/index.test.ts \
  packages/core-tenancy/tests/isolation.test.ts \
  packages/core-tenancy/tests/tenant-encryption.test.ts \
  packages/core-telemetry/tests/index.test.ts

run_batch "core-secrets+rbac+policy" \
  packages/core-secrets/tests/sigv4.test.ts \
  packages/core-secrets/tests/redaction.test.ts \
  packages/core-secrets/tests/tenant-key.test.ts \
  packages/core-rbac/tests/index.test.ts \
  packages/core-rbac/tests/jwt-verify.test.ts \
  packages/core-policy/tests/index.test.ts \
  packages/core-policy/tests/signed-policy.test.ts

run_batch "core-events+enterprise+sandbox" \
  packages/core-events/tests/index.test.ts \
  packages/core-events/tests/audit.test.ts \
  packages/core-events/tests/worm-audit.test.ts \
  packages/core-enterprise/tests/index.test.ts \
  packages/core-enterprise/tests/quotas.test.ts \
  packages/core-enterprise/tests/quota-persistence.test.ts \
  packages/core-enterprise/tests/enterprise-integration.test.ts \
  packages/core-sandbox/tests/index.test.ts \
  packages/core-sandbox/tests/runner.test.ts \
  packages/core-sandbox/tests/escape-prevention.test.ts \
  packages/core-flags/tests/index.test.ts

run_batch "lint-tools" \
  packages/tool-lint-ts/tests/index.test.ts \
  packages/tool-lint-py/tests/index.test.ts \
  packages/tool-lint-sh/tests/index.test.ts \
  packages/tool-lint-c/tests/index.test.ts \
  packages/tool-lint-rs/tests/index.test.ts \
  packages/tool-lint-go/tests/index.test.ts \
  packages/tool-lint-sql/tests/index.test.ts \
  packages/tool-lint-core/tests/index.test.ts \
  packages/tool-lint-imports/tests/python-renames.test.ts \
  packages/tool-lint-imports/tests/typescript-renames.test.ts \
  packages/tool-lint-imports/tests/clean-pass.test.ts \
  packages/tool-pyright/tests/index.test.ts \
  packages/tool-tsc/tests/index.test.ts

run_batch "memory+model" \
  packages/memory-palace/tests/index.test.ts \
  packages/memory-palace/tests/sqlite-adapter.test.ts \
  packages/memory-palace/tests/sqlite-backup.test.ts \
  packages/model-config/tests/config-loader.test.ts \
  packages/model-client/tests/index.test.ts

run_batch "agent+prompt+correction" \
  packages/agent-core/tests/index.test.ts \
  packages/prompt-core/tests/index.test.ts \
  packages/correction-core/tests/index.test.ts \
  packages/correction-core/tests/boundary.test.ts \
  packages/correction-core/tests/task-validator.test.ts \
  packages/correction-core/tests/bench-normalize.test.ts

run_batch "plugin+orch" \
  packages/plugin/tests/index.test.ts \
  packages/plugin/tests/auth-audit-bridge.test.ts \
  packages/plugin/tests/auth-chain-integration.test.ts \
  packages/orchestrator/tests/index.test.ts \
  packages/orchestrator/tests/health-server.test.ts \
  packages/orchestrator/tests/validator.test.ts \
  packages/orchestrator/tests/validator-contract.test.ts \
  packages/orchestrator/tests/verifier-fail-closed.test.ts \
  packages/orchestrator/tests/coverage.test.ts \
  packages/orchestrator/tests/decompose.test.ts \
  packages/orchestrator/tests/chaos.test.ts

run_batch "cli+e2e" \
  apps/cli/tests/cli-commands.test.ts \
  apps/cli/tests/doctor.test.ts \
  apps/cli/tests/observe.test.ts \
  e2e/smoke.test.ts

run_batch "load-baseline" \
  tests/load/memory-palace-load.test.ts \
  tests/load/slo-monitor-load.test.ts \
  tests/load/enterprise-load.test.ts

echo ""
[ "$FAIL" -eq 0 ] && echo "ALL TESTS PASSED" || echo "SOME TESTS FAILED"
exit $FAIL
