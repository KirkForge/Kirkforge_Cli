import type { LintRule } from "@kirkforge/tool-lint-core";

export const correctRules: LintRule[] = [
  {
    id: "no-duplicate-key",
    category: "correct",
    severity: "high",
    pattern: /\{[^}]*\b(\w+)\s*:\s*[^,}]+,\s*[^}]*\b\1\s*:/g,
    message: "Duplicate dictionary key; later value overwrites earlier",
  },
  {
    id: "no-assert-on-tuple",
    category: "correct",
    severity: "high",
    pattern: /\bassert\s*\([^,)]+,[^)]+\)/g,
    message:
      "assert(x, y) is always truthy; assert separately or use parentheses for the condition",
  },
  {
    id: "no-incorrect-type-is",
    category: "correct",
    severity: "med",
    pattern: /\btype\s*\(\s*\w+\s*\)\s*==\s*/g,
    message: "Use isinstance() instead of type() == for type checking",
  },
  {
    id: "no-unused-import",
    category: "correct",
    severity: "med",
    pattern: /^import\s+(\w+)(?:\s+as\s+\w+)?\s*$(?![^]*?\b\1\b)/gm,
    message: "Import may be unused; remove or verify usage",
  },
  {
    id: "no-redefined-outer",
    category: "correct",
    severity: "med",
    pattern: /def \w+\([^)]*\).*\n(?:.*\n)*?^\s+(\w+)\s*=/gm,
    message: "Local variable may shadow outer scope name",
  },
  {
    id: "no-undefined-var",
    category: "correct",
    severity: "high",
    pattern: /\bNameError\b/g,
    message: "Potential NameError; verify all variables are defined before use",
  },
  {
    id: "no-self-cls-mismatch",
    category: "correct",
    severity: "med",
    pattern: /def (?!__init__)\w+\(self[^)]*\)\s*:.*\bcls\b/g,
    message: "Instance method uses 'cls' instead of 'self'",
  },
];
