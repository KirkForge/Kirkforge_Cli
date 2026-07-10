import type { LintRule } from "@kirkforge/tool-lint-core";

export const perfRules: LintRule[] = [
  {
    id: "no-range-len",
    category: "perf",
    severity: "med",
    pattern: /for\s+\w+\s+in\s+range\s*\(\s*len\s*\(/g,
    message: "Use enumerate() instead of range(len())",
  },
  {
    id: "no-list-in-loop",
    category: "perf",
    severity: "med",
    pattern: /^\s*\.append\(/gm,
    message: "Building a list with .append() in a loop; use a list comprehension",
  },
  {
    id: "no-dict-keys-iterate",
    category: "perf",
    severity: "low",
    pattern: /for\s+\w+\s+in\s+\w+\.keys\s*\(\s*\)\s*:/g,
    message: "Iterate dict directly with 'for k in d' instead of 'for k in d.keys()'",
  },
  {
    id: "no-string-concat-loop",
    category: "perf",
    severity: "med",
    pattern: /\w+\s*\+=\s*['"]/g,
    message: "String concatenation in loop; use ''.join() instead",
  },
  {
    id: "no-items-iterate",
    category: "perf",
    severity: "low",
    pattern: /for\s+\w+\s*,\s*\w+\s+in\s+\w+\.items\s*\(/g,
    message: "Iterate with .items() instead of .keys() and lookup",
  },
];
