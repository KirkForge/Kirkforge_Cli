#!/usr/bin/env bash
# Map changed files (base..HEAD) to affected cargo packages, including the
# reverse-dependency closure (if a dependency crate changes, every crate that
# depends on it is affected too). Used by ci-pr.yml to run tests only for
# changed packages (WO 33.6).
#
# Usage:
#   scripts/changed-packages.sh [base-ref]
#     base-ref defaults to origin/dev.
#
# Output (stdout): a single space-separated list of cargo package names,
#   suitable for `cargo nextest run -p <pkg> ...`. Empty when no Rust
#   packages are affected (caller skips the test step).
#   A literal `__NO_RUST_CHANGES__` token is printed only when the change
#   set touches no Rust-relevant paths at all, so callers can distinguish
#   "docs-only" from "nothing changed".
#
# Exit codes: 0 on success (the "no changes" case is a normal result, not an
#   error). Non-zero on git/usage failure — the caller must propagate this
#   (WO 44.52): swallowing it turns a classifier crash into a silently
#   untested PR.
#
# ponytail: the reverse-dep table is hardcoded. The workspace is small and
#   stable (13 crates, 4 inter-crate edges); a `cargo metadata` subprocess
#   would be 10x the code for a graph that changes once a quarter. Ceiling:
#   if a crate gains a new internal dep, bump the table below or the script
#   will under-report the affected set (the nextest run would still pass —
#   just run fewer packages than ideal). Upgrade path: parse `cargo metadata`
#   --format-version 1` if the workspace grows past ~30 crates or gains
#   optional/feature-gated internal deps that change per-feature.

set -euo pipefail

BASE_REF="${1:-origin/dev}"
cd "$(dirname "$0")/.."

# ── Internal reverse-dependency adjacency table ─────────────────────────
# Format: "<dep> <reverse-dep>..." — "if <dep> changes, also test these".
# The root `kf-code` crate depends on EVERY workspace member (root
# Cargo.toml), so any crate change pulls in kf-code automatically via the
# catch-all at the bottom. Only the *additional* inter-crate edges are
# listed here.
#
# Known edges (verified against crates/*/Cargo.toml + root Cargo.toml):
#   kf-memory-store  -> kf-routing
#   kf-orchestrator  -> kf-routing, kf-memory-store
#   kf-plugin-host   -> kf-plugin-sdk
#
# Reverse-deps (dep changed  =>  also affected):
reverse_deps_of() {
    case "$1" in
        kf-routing)        echo "kf-memory-store kf-orchestrator" ;;
        kf-memory-store)   echo "kf-orchestrator" ;;
        kf-plugin-sdk)     echo "kf-plugin-host" ;;
        *)                 echo "" ;;
    esac
}

# ── Gather changed files ────────────────────────────────────────────────
# If git diff fails (base ref not resolvable — e.g. fork PRs where
# origin/${base_ref} doesn't exist), fail loudly. A silent empty result would
# be indistinguishable from "no Rust changes" and make the PR check suite
# green with zero Rust checks run (WO 44.52). The caller (ci-pr.yml) no longer
# swallows this exit code.
if ! CHANGED=$(git diff --name-only --diff-filter=ACMR "$BASE_REF..HEAD"); then
    echo "changed-packages: FAILED to resolve base ref '$BASE_REF' — see git error above." >&2
    echo "changed-packages: this is a classification error, not a clean tree (WO 44.52)." >&2
    exit 1
fi

if [ -z "$CHANGED" ]; then
    echo "changed-packages: no changes vs $BASE_REF." >&2
    echo ""
    exit 0
fi

# ── Path filters: classify the change set ──────────────────────────────
# Rust-relevant: .rs, Cargo.toml, Cargo.lock, build.rs, plus anything under
#   benches/ or tests/ (may be .rs or fixtures, all build-relevant).
# Docs-only: docs/** or any *.md anywhere. A change that is *purely* docs
#   skips Rust CI entirely. A mixed change (docs + src) runs Rust CI on the
#   src portion — only the docs files are ignored by the package mapper.
is_rust_path() {
    case "$1" in
        *.rs|Cargo.toml|Cargo.lock|build.rs) return 0 ;;
        benches/*|tests/*|crates/*/benches/*|crates/*/tests/*) return 0 ;;
        crates/*/Cargo.toml) return 0 ;;
        *) return 1 ;;
    esac
}

is_docs_only_path() {
    case "$1" in
        docs/*|*.md|*/docs/*.md) return 0 ;;
        *) return 1 ;;
    esac
}

rust_files=""
docs_files=""
other_files=""
while IFS= read -r f; do
    if is_rust_path "$f"; then
        rust_files+="$f"$'\n'
    elif is_docs_only_path "$f"; then
        docs_files+="$f"$'\n'
    else
        other_files+="$f"$'\n'
    fi
done <<< "$CHANGED"

# Docs-only change set => skip Rust CI entirely.
if [ -z "$rust_files" ] && [ -z "$other_files" ] && [ -n "$docs_files" ]; then
    echo "changed-packages: docs-only change; skipping Rust CI." >&2
    echo "__NO_RUST_CHANGES__"
    exit 0
fi

# No Rust-relevant files at all (and not docs-only, e.g. scripts/ change).
if [ -z "$rust_files" ]; then
    echo "changed-packages: no Rust-relevant files changed." >&2
    echo "__NO_RUST_CHANGES__"
    exit 0
fi

# ── Map changed files to packages ──────────────────────────────────────
# A package is identified by its cargo name (root = kf-code; crates/ dir
# name is the cargo name verbatim — all crates use kf-<dir> form).
map_to_package() {
    case "$1" in
        crates/*)   echo "${1#crates/}" | cut -d/ -f1 ;;
        src/*|tests/*|benches/*|Cargo.toml|Cargo.lock|build.rs) echo "kf-code" ;;
        *) echo "" ;;
    esac
}

declare -A seen
affected=()
add_pkg() {
    local pkg="$1"
    [ -z "$pkg" ] && return
    if [ -z "${seen[$pkg]:-}" ]; then
        seen[$pkg]=1
        affected+=("$pkg")
        # expand reverse-dep closure
        local rev
        rev=$(reverse_deps_of "$pkg")
        for r in $rev; do
            add_pkg "$r"
        done
    fi
}

while IFS= read -r f; do
    [ -z "$f" ] && continue
    add_pkg "$(map_to_package "$f")"
done <<< "$rust_files"

# Always include kf-code if any crate changed, because kf-code depends on
# every workspace member (root Cargo.toml). The root's own files already
# mapped to kf-code above; this covers the "only a crate changed" case.
for f in $rust_files; do
    case "$f" in
        crates/*) add_pkg "kf-code" ;;
    esac
done

if [ "${#affected[@]}" -eq 0 ]; then
    echo "changed-packages: no package mapping applied." >&2
    echo ""
    exit 0
fi

# ── Output ─────────────────────────────────────────────────────────────
# stderr: human summary; stdout: machine-readable space-separated list.
printf 'changed-packages: %d package(s) affected: %s\n' \
    "${#affected[@]}" "${affected[*]}" >&2
echo "${affected[*]}"