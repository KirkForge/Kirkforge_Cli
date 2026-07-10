#!/usr/bin/env python3
"""Extract the release notes for a given tag from CHANGELOG.md.

Usage:
    scripts/extract-release-notes.py v0.2.0

Writes the matching version section to stdout. Exits with status 1 if the
section is not found.
"""
import re
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("Usage: extract-release-notes.py vX.Y.Z", file=sys.stderr)
        return 1

    tag = sys.argv[1]
    if not tag.startswith("v"):
        print(f"error: expected tag like vX.Y.Z, got {tag}", file=sys.stderr)
        return 1

    version = tag[1:]
    with open("CHANGELOG.md", encoding="utf-8") as f:
        text = f.read()

    # Match a header like "## [0.2.0] - 2026-07-04".
    pattern = re.compile(rf"^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}\s*\n", re.MULTILINE)
    match = pattern.search(text)
    if not match:
        print(f"error: no changelog section found for {tag}", file=sys.stderr)
        return 1

    start = match.end()
    next_header = re.search(r"\n## ", text[start:])
    end = start + next_header.start() if next_header else len(text)

    notes = text[start:end].strip()
    if not notes:
        print(f"warning: empty release notes for {tag}", file=sys.stderr)
    print(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
