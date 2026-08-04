#!/usr/bin/env bash
# Validator for task 01-typescript-bug-fix.
# The harness invokes this script with CWD set to the worker's isolated
# workspace, so all paths are relative to that workspace.
set -euo pipefail

# 1. File must exist
if [ ! -f src/clamp.ts ]; then
  echo "FAIL: src/clamp.ts missing (PWD=$PWD)"
  exit 1
fi

# 2. Boundary cases — these are the cases that exercise the bug
npx tsx -e "
import { clamp } from './src/clamp.ts';
const cases = [
  { args: [0, 0, 10], expected: 0 },
  { args: [10, 0, 10], expected: 10 },
  { args: [5, 0, 10], expected: 5 },
  { args: [-1, 0, 10], expected: 0 },
  { args: [11, 0, 10], expected: 10 },
  { args: [-5, -3, 3], expected: -3 },
  { args: [7, 0, 7], expected: 7 },
  { args: [3.14, 0, 1], expected: 1 },
];
let failed = 0;
for (const c of cases) {
  const got = clamp(...c.args);
  if (Math.abs(got - c.expected) > 1e-9) {
    console.error('FAIL: clamp(' + c.args.join(',') + ') = ' + got + ', expected ' + c.expected);
    failed++;
  }
}
if (failed > 0) {
  console.error(failed + ' test(s) failed');
  process.exit(1);
}
console.log('PASS: all ' + cases.length + ' boundary cases');
"
