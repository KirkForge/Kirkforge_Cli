#!/usr/bin/env bash
# Bump crate version, refresh Cargo.lock, prep CHANGELOG, and create an annotated tag.
# Usage: scripts/bump-version.sh 0.2.0

set -euo pipefail

NEW_VERSION="${1:-}"
if [[ ! "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Usage: $0 <major.minor.patch>" >&2
    exit 1
fi

cd "$(dirname "$0")/.."
TODAY=$(date +%Y-%m-%d)

# Update the version line in the root Cargo.toml.
python3 - "$NEW_VERSION" <<'PY'
import re, sys
v = sys.argv[1]
with open('Cargo.toml') as f:
    text = f.read()
new = re.sub(r'^(version = )"[^"]+"', rf'\1"{v}"', text, count=1, flags=re.M)
if new == text:
    print('error: Cargo.toml version line not found', file=sys.stderr)
    sys.exit(1)
with open('Cargo.toml', 'w') as f:
    f.write(new)
PY

echo "Bumped Cargo.toml version to $NEW_VERSION"

# Refresh Cargo.lock so the new version is recorded.
# --locked is intentionally omitted: bumping the workspace version makes the
# previous lockfile stale, and we want Cargo to rewrite it before the commit.
cargo check
echo "Refreshed Cargo.lock"

# Split the current Unreleased section into a new empty Unreleased plus a versioned section.
python3 - "$NEW_VERSION" "$TODAY" <<'PY'
import re, sys
v, d = sys.argv[1], sys.argv[2]
with open('CHANGELOG.md') as f:
    text = f.read()
needle = '## [Unreleased]\n'
idx = text.find(needle)
if idx == -1:
    print('error: CHANGELOG.md must contain ## [Unreleased]', file=sys.stderr)
    sys.exit(1)
insert_pos = idx + len(needle)
new_text = text[:insert_pos] + f'\n## [{v}] - {d}\n' + text[insert_pos:]
with open('CHANGELOG.md', 'w') as f:
    f.write(new_text)
PY

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): bump version to $NEW_VERSION"
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"

echo "Created annotated tag v$NEW_VERSION. Push it to trigger the release workflow:"
echo "  git push origin v$NEW_VERSION"
