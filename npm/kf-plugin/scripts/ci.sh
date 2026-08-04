#!/usr/bin/env bash
# ci-cleandev v3 — pre-push CI runner
# Auto-detects project type, package manager, and available scripts.
# Reads .ci-cleandev.yml for per-project configuration.
set -uo pipefail

# ─── Colors ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; DIM='\033[2m'; NC='\033[0m'

# ─── Counters ────────────────────────────────────────────────────────────────
PASS=0; FAIL=0; SKIP=0; WARN=0

# ─── Defaults ─────────────────────────────────────────────────────────────────
TIMEOUT="${CI_CLEANDEV_TIMEOUT:-600}"
REQUIRE_TRUFFLEHOG="${CI_CLEANDEV_REQUIRE_TRUFFLEHOG:-0}"
CI_MODE="normal"
CONFIG_FILE=""
PROJECT_NAME="$(basename "$(pwd)")"
PM_AVAILABLE=1
MAX_OUTPUT_LINES=30

# ─── Resolve python command ─────────────────────────────────────────────────
PYTHON_CMD=""
resolve_python() {
    if command -v python3 >/dev/null 2>&1; then
        PYTHON_CMD="python3"
    elif command -v python >/dev/null 2>&1; then
        PYTHON_CMD="python"
    fi
}

# ─── Config loader ───────────────────────────────────────────────────────────
load_config() {
    local conf
    for conf in .ci-cleandev.yml .ci-cleandev; do
        if [ -f "$conf" ]; then
            CONFIG_FILE="$conf"
            break
        fi
    done

    if [ -z "$CONFIG_FILE" ]; then
        return
    fi

    while IFS= read -r line; do
        line="${line%%#*}"
        line="${line%%;*}"
        line="$(echo "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        [ -z "$line" ] && continue

        key="${line%%=*}"
        val="${line#*=}"
        key="$(echo "$key" | sed 's/[[:space:]]*//')"
        val="$(echo "$val" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"

        case "$key" in
            timeout)            TIMEOUT="$val" ;;
            mode)               CI_MODE="$val" ;;
            require_trufflehog)  REQUIRE_TRUFFLEHOG="$val" ;;
        esac
    done < "$CONFIG_FILE"
}

# ─── Step runner ──────────────────────────────────────────────────────────────
run_step() {
    local label="$1"; shift
    local log_file
    log_file="$(mktemp -t ci-cleandev.XXXXXX.log)"

    printf "  %-42s " "$label"

    local exit_code=0
    timeout "$TIMEOUT" "$@" > "$log_file" 2>&1 || exit_code=$?

    if [ "$exit_code" -eq 0 ]; then
        echo -e "${GREEN}PASS${NC}"
        PASS=$((PASS + 1))
        rm -f "$log_file"
        return 0
    else
        if [ "$exit_code" -eq 124 ]; then
            echo -e "${RED}TIMEOUT${NC} (${TIMEOUT}s)"
        else
            echo -e "${RED}FAIL${NC}"
        fi
        local line_count
        line_count="$(wc -l < "$log_file")"
        echo -e "${DIM}----- $label output ($line_count lines) -----${NC}"
        if [ "$line_count" -gt "$MAX_OUTPUT_LINES" ]; then
            head -n "$MAX_OUTPUT_LINES" "$log_file"
            echo -e "${DIM}... ($((line_count - MAX_OUTPUT_LINES)) more lines, see full log above)${NC}"
        else
            cat "$log_file"
        fi
        echo -e "${DIM}--------------------------${NC}"
        echo -e "  exit code: ${exit_code}"
        FAIL=$((FAIL + 1))
        rm -f "$log_file"
        return 1
    fi
}

skip_step() {
    local label="$1"
    local reason="${2:-}"
    printf "  %-42s " "$label"
    if [ -n "$reason" ]; then
        echo -e "${YELLOW}SKIP${NC} ${DIM}($reason)${NC}"
    else
        echo -e "${YELLOW}SKIP${NC}"
    fi
    SKIP=$((SKIP + 1))
}

warn_step() {
    local label="$1"
    local msg="${2:-}"
    printf "  %-42s " "$label"
    echo -e "${YELLOW}WARN${NC} ${DIM}$msg${NC}"
    WARN=$((WARN + 1))
}

fail_step() {
    local label="$1"
    local msg="${2:-}"
    printf "  %-42s " "$label"
    echo -e "${RED}FAIL${NC} $msg"
    FAIL=$((FAIL + 1))
}

# ─── Package-manager detection ───────────────────────────────────────────────
detect_pm() {
    if [ -f pnpm-lock.yaml ]; then echo "pnpm"
    elif [ -f yarn.lock ]; then echo "yarn"
    elif [ -f bun.lockb ]; then echo "bun"
    else echo "npm"
    fi
}

pm_is_installed() {
    command -v "$1" >/dev/null 2>&1
}

# ─── Script detection ────────────────────────────────────────────────────────
has_npm_script() {
    local script="$1"
    [ -f package.json ] || return 1
    node -e "const p=require('./package.json'); process.exit(p.scripts && p.scripts['$script'] ? 0 : 1)" 2>/dev/null
}

# ─── Install deps if needed ──────────────────────────────────────────────────
ensure_node_deps() {
    local pm
    pm="$(detect_pm)"

    if ! pm_is_installed "$pm"; then
        fail_step "install ($pm)" "— $pm not installed. Install it or switch lockfiles."
        PM_AVAILABLE=0
        return
    fi
    PM_AVAILABLE=1

    if [ ! -d node_modules ]; then
        case "$pm" in
            pnpm) run_step "install ($pm)" pnpm install --frozen-lockfile ;;
            yarn) run_step "install ($pm)" yarn install --frozen-lockfile ;;
            bun)  run_step "install ($pm)" bun install ;;
            npm)  run_step "install ($pm)" npm ci ;;
        esac
    else
        local lockfile=""
        case "$pm" in
            pnpm) lockfile="pnpm-lock.yaml" ;;
            yarn) lockfile="yarn.lock" ;;
            bun)  lockfile="bun.lockb" ;;
            npm)  lockfile="package-lock.json" ;;
        esac

        if [ -n "$lockfile" ] && [ -f "$lockfile" ] && [ "$lockfile" -nt node_modules ] 2>/dev/null; then
            case "$pm" in
                pnpm) run_step "install ($pm)" pnpm install --frozen-lockfile ;;
                yarn) run_step "install ($pm)" yarn install --frozen-lockfile ;;
                bun)  run_step "install ($pm)" bun install ;;
                npm)  run_step "install ($pm)" npm ci ;;
            esac
        else
            skip_step "install ($pm)" "node_modules up to date"
        fi
    fi
}

# ─── Project type detection ──────────────────────────────────────────────────
is_python_project() {
    [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f setup.py ] || [ -f Pipfile ]
}

is_docker_project() {
    [ -f Dockerfile ]
}

has_github_actions() {
    [ -d .github/workflows ]
}

# ─── Watch-mode detection for test scripts ───────────────────────────────────
is_watch_test() {
    local test_cmd
    test_cmd="$(node -e "const p=require('./package.json'); console.log(p.scripts.test)" 2>/dev/null || true)"
    echo "$test_cmd" | grep -qiE '(vitest|jest).*--watch|watch.*(vitest|jest)|--watchAll'
}

# ─── Trufflehog exclude file ─────────────────────────────────────────────────
# Creates a temp file with exclusion patterns for trufflehog's --exclude-paths flag
create_trufflehog_excludes() {
    local exclude_file
    exclude_file="$(mktemp -t ci-cleandev-trufflehog-excludes.XXXXXX)"
    cat > "$exclude_file" << 'EXCLUDES'
^node_modules/
^\.git/
^dist/
^build/
^\.next/
^coverage/
^__pycache__/
^\.tox/
^venv/
^\.venv/
^\.cache/
^vendor/
EXCLUDES
    echo "$exclude_file"
}

# ─── Main ─────────────────────────────────────────────────────────────────────
load_config
resolve_python

echo ""
echo -e "${BOLD}${YELLOW}╔══════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}${YELLOW}║${NC}  ${BOLD}CI: $PROJECT_NAME${NC}"
echo -e "${BOLD}${YELLOW}╚══════════════════════════════════════════════════╝${NC}"

if [ -n "$CONFIG_FILE" ]; then
    echo -e "  ${DIM}config: $CONFIG_FILE (mode=$CI_MODE, timeout=${TIMEOUT}s)${NC}"
fi

# ─── Node / JS / TS ─────────────────────────────────────────────────────────
if [ -f package.json ]; then
    echo -e "${BOLD}[node]${NC}"
    pm="$(detect_pm)"
    echo -e "  ${DIM}package manager: $pm${NC}"

    ensure_node_deps

    if [ "$PM_AVAILABLE" -eq 0 ]; then
        warn_step "node steps" "skipped — $pm not installed"
    else
        # Typecheck
        if has_npm_script "typecheck"; then
            run_step "typecheck" $pm run typecheck
        elif has_npm_script "check"; then
            run_step "typecheck" $pm run check
        fi

        # Lint
        if has_npm_script "lint"; then
            run_step "lint" $pm run lint
        fi

        # Format check
        if has_npm_script "format:check"; then
            run_step "format" $pm run format:check
        elif has_npm_script "formatting"; then
            run_step "format" $pm run formatting
        fi

        # Test — prefer test:ci over test (avoids watch-mode hangs)
        # test:ci uses its own per-batch timeouts, so we skip the ci.sh wrapper timeout
        if has_npm_script "test:ci"; then
            test_exit=0
            test_log="$(mktemp -t ci-cleandev-test.XXXXXX.log)"
            printf "  %-42s " "test"
            $pm run test:ci > "$test_log" 2>&1 || test_exit=$?
            if [ "$test_exit" -eq 0 ]; then
                echo -e "${GREEN}PASS${NC}"
                PASS=$((PASS + 1))
                rm -f "$test_log"
            else
                if [ "$test_exit" -eq 124 ]; then
                    echo -e "${RED}TIMEOUT${NC} (ci.sh timeout ${TIMEOUT}s)"
                else
                    echo -e "${RED}FAIL${NC}"
                fi
                tail -20 "$test_log"
                echo -e "  exit code: ${test_exit}"
                FAIL=$((FAIL + 1))
                rm -f "$test_log"
            fi
        elif has_npm_script "test"; then
            if is_watch_test; then
                warn_step "test" "watch mode detected — forcing single run"
                test_cmd="$(node -e "const p=require('./package.json'); console.log(p.scripts.test)" 2>/dev/null || true)"
                if echo "$test_cmd" | grep -q 'vitest'; then
                    run_step "test" npx vitest run
                elif echo "$test_cmd" | grep -q 'jest'; then
                    run_step "test" npx jest --ci
                else
                    run_step "test" $pm test
                fi
            else
                run_step "test" $pm test
            fi
        fi

        # Additional test suites
        if has_npm_script "test:adapter"; then
            run_step "test:adapter" $pm run test:adapter
        fi

        # Build
        if has_npm_script "build"; then
            run_step "build" $pm run build
        elif has_npm_script "compile"; then
            run_step "build" $pm run compile
        fi

        # Audit
        if [ "$CI_MODE" != "fast" ]; then
            case "$pm" in
                npm)
                    if [ -f package-lock.json ]; then
                        run_step "npm audit" npm audit --omit=dev --audit-level=high
                    else
                        skip_step "npm audit" "no lockfile"
                    fi
                    ;;
                pnpm)
                    if [ -f pnpm-lock.yaml ]; then
                        run_step "pnpm audit" pnpm audit --audit-level=high
                    else
                        skip_step "pnpm audit" "no lockfile"
                    fi
                    ;;
            esac
        fi
    fi
fi

# ─── Python ───────────────────────────────────────────────────────────────────
if is_python_project; then
    echo -e "${BOLD}[python]${NC}"

    if [ -z "$PYTHON_CMD" ]; then
        skip_step "pytest" "python not found"
        skip_step "ruff" "python not found"
        skip_step "mypy" "python not found"
    else
        if "$PYTHON_CMD" -m pytest --version >/dev/null 2>&1; then
            run_step "pytest" "$PYTHON_CMD" -m pytest -x --tb=short
        else
            skip_step "pytest" "not installed (pip install pytest)"
        fi

        if command -v ruff >/dev/null 2>&1; then
            run_step "ruff" ruff check .
        else
            skip_step "ruff" "not installed (pip install ruff)"
        fi

        if "$PYTHON_CMD" -m mypy --version >/dev/null 2>&1; then
            run_step "mypy" "$PYTHON_CMD" -m mypy . --ignore-missing-imports
        else
            skip_step "mypy" "not installed (pip install mypy)"
        fi
    fi
fi

# ─── Docker ───────────────────────────────────────────────────────────────────
if is_docker_project; then
    echo -e "${BOLD}[docker]${NC}"
    if [ "$CI_MODE" = "fast" ]; then
        skip_step "docker build" "fast mode"
    elif command -v docker >/dev/null 2>&1; then
        run_step "docker build" docker build -t "ci-cleandev-$(echo "$PROJECT_NAME" | tr '[:upper:]' '[:lower:]')" .
    else
        skip_step "docker build" "docker not available"
    fi
fi

# ─── GitHub Actions ──────────────────────────────────────────────────────────
if has_github_actions; then
    echo -e "${BOLD}[github actions]${NC}"
    if [ "$CI_MODE" = "fast" ]; then
        skip_step "actionlint" "fast mode"
    elif command -v actionlint >/dev/null 2>&1; then
        run_step "actionlint" actionlint
    else
        skip_step "actionlint" "not installed (brew install actionlint)"
    fi
fi

# ─── Secret scan ─────────────────────────────────────────────────────────────
if [ "$CI_MODE" != "fast" ]; then
    echo -e "${BOLD}[security]${NC}"
    SECRET_SCANNERS=0

    # gitleaks: primary scanner — uses per-repo .gitleaks.toml rules
    if command -v gitleaks >/dev/null 2>&1; then
        GL_ARGS="detect --source . --redact --exit-code 1"
        if [ -f .gitleaks.toml ]; then
            GL_ARGS="$GL_ARGS --config .gitleaks.toml"
        fi
        run_step "secrets (gitleaks)" gitleaks $GL_ARGS
        SECRET_SCANNERS=$((SECRET_SCANNERS + 1))
    fi

    # trufflehog: secondary scanner — catches different patterns
    if command -v trufflehog >/dev/null 2>&1; then
        THOG_EXCLUDE_FILE="$(create_trufflehog_excludes)"
        run_step "secrets (trufflehog)" trufflehog filesystem . --no-update --fail --exclude-paths="$THOG_EXCLUDE_FILE" --filter-unverified
        rm -f "$THOG_EXCLUDE_FILE"
        SECRET_SCANNERS=$((SECRET_SCANNERS + 1))
    fi

    if [ "$SECRET_SCANNERS" -eq 0 ]; then
        if [ "$REQUIRE_TRUFFLEHOG" = "1" ]; then
            fail_step "secrets" "(no scanner installed — install gitleaks or trufflehog)"
        else
            skip_step "secrets" "no scanner installed (install gitleaks or trufflehog)"
        fi
    fi
fi

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}─────────────────────────────────────────────────────${NC}"
if [ "$FAIL" -gt 0 ]; then
    echo -e "${RED}${BOLD}  FAIL${NC}  ${RED}$FAIL failed${NC}, $PASS passed, $SKIP skipped, $WARN warnings"
    echo ""
    exit 1
else
    echo -e "${GREEN}${BOLD}  PASS${NC}  $PASS passed, $SKIP skipped, $WARN warnings"
    echo ""
    exit 0
fi
