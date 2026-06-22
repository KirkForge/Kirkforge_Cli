#!/usr/bin/env bash
# Run the Ollama-backed integration test suite.
#
# Requirements:
#   - Ollama running on http://localhost:11434
#   - qwen2.5:0.5b pulled (ollama pull qwen2.5:0.5b)
#
# The integration tests are marked #[ignore] so they don't run during
# normal `cargo test`. Use this script (or pass --include-ignored) to
# run them explicitly.

set -euo pipefail

OLLAMA_HOST="${OLLAMA_HOST:-http://localhost:11434}"
TEST_MODEL="qwen2.5:0.5b"

if ! curl -fsS "${OLLAMA_HOST}/api/tags" >/dev/null 2>&1; then
    echo "ERROR: no Ollama server at ${OLLAMA_HOST}"
    echo "Start one with: ollama serve"
    exit 1
fi

if ! curl -fsS "${OLLAMA_HOST}/api/tags" | grep -q "${TEST_MODEL}"; then
    echo "ERROR: test model ${TEST_MODEL} is not pulled"
    echo "Pull it with: ollama pull ${TEST_MODEL}"
    exit 1
fi

echo "Ollama reachable and model present — running integration tests..."
cargo test --test integration_test -- --include-ignored --nocapture "$@"
