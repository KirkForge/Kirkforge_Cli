#!/usr/bin/env bash
# Validator for task 02-typescript-add-function.
# The harness invokes this script with CWD set to the worker's isolated
# workspace, so all paths are relative to that workspace.
set -euo pipefail

if [ ! -f src/array-utils.ts ]; then
  echo "FAIL: src/array-utils.ts missing (PWD=$PWD)"
  exit 1
fi

npx tsx -e "
import * as mod from './src/array-utils.ts';

// 1. Existing exports must be present
if (typeof mod.first !== 'function') {
  console.error('FAIL: first() missing or not a function');
  process.exit(1);
}
if (typeof mod.last !== 'function') {
  console.error('FAIL: last() missing or not a function');
  process.exit(1);
}
if (mod.first([1,2,3]) !== 1) { console.error('FAIL: first() broken'); process.exit(1); }
if (mod.last([1,2,3]) !== 3) { console.error('FAIL: last() broken'); process.exit(1); }

// 2. unique must exist
if (typeof mod.unique !== 'function') {
  console.error('FAIL: unique() not exported');
  process.exit(1);
}

// 3. unique must not use Set internally
const src = require('fs').readFileSync('src/array-utils.ts', 'utf8');
if (/\bnew\s+Set\b/.test(src)) {
  console.error('FAIL: unique() uses new Set (forbidden)');
  process.exit(1);
}

// 4. unique correctness — primitive cases only. Object identity is
// reference-based, so we don't test object dedup.
const cases = [
  { input: [1, 1, 2, 2, 3], expected: [1, 2, 3] },
  { input: ['a', 'b', 'a', 'c', 'b'], expected: ['a', 'b', 'c'] },
  { input: [], expected: [] },
  { input: [1, 2, 3], expected: [1, 2, 3] },
  { input: [null, null, undefined, undefined, 0], expected: [null, undefined, 0] },
  { input: [true, false, true, true, false], expected: [true, false] },
];
let failed = 0;
for (const c of cases) {
  const got = mod.unique(c.input);
  if (JSON.stringify(got) !== JSON.stringify(c.expected)) {
    console.error('FAIL: unique(' + JSON.stringify(c.input) + ') = ' + JSON.stringify(got) + ', expected ' + JSON.stringify(c.expected));
    failed++;
  }
}
if (failed > 0) {
  console.error(failed + ' test(s) failed');
  process.exit(1);
}
console.log('PASS: all ' + cases.length + ' cases + existing exports');
"
