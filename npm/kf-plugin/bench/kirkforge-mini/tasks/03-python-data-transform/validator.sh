#!/usr/bin/env bash
# Validator for task 03-python-data-transform.
# The harness invokes this script with CWD set to the worker's isolated
# workspace, so all paths are relative to that workspace.
set -euo pipefail

if [ ! -f normalize.py ]; then
  echo "FAIL: normalize.py missing (PWD=$PWD)"
  exit 1
fi

python3 -c "
import sys
sys.path.insert(0, '.')
from normalize import normalize_name

cases = [
    ('  Hello World  ', 'hello_world'),
    ('FOO_BAR_baz', 'foo_bar_baz'),
    ('  spaces  everywhere  ', 'spaces_everywhere'),
    ('Tabs\tand\nnewlines', 'tabs_and_newlines'),
    ('---dashes---', 'dashes'),
    ('mixedCASE123', 'mixedcase123'),
    ('', ''),
    ('   ', ''),
    ('already_clean', 'already_clean'),
    ('a___b____c', 'a_b_c'),
    ('ALPHA__BETA', 'alpha_beta'),
]
failed = 0
for inp, expected in cases:
    got = normalize_name(inp)
    if got != expected:
        print(f'FAIL: normalize_name({inp!r}) = {got!r}, expected {expected!r}')
        failed += 1
if failed > 0:
    print(f'{failed} test(s) failed')
    sys.exit(1)
print(f'PASS: all {len(cases)} cases')
"
