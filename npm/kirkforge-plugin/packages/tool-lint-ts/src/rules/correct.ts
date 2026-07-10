import type { LintRule } from "@kirkforge/tool-lint-core";

export const correctRules: LintRule[] = [
  {
    id: "no-eq-null",
    category: "correct",
    severity: "low",
    pattern: /[!=]=\s*\bnull\b/g,
    message: "Consider === null or !== null instead of == null / != null",
  },
  {
    id: "no-return-await",
    category: "correct",
    severity: "low",
    pattern: /\breturn\s+await\b/g,
    message: "Redundant await in return; just return the promise",
  },
  {
    id: "no-throw-literal",
    category: "correct",
    severity: "info",
    pattern: /\bthrow\s+(?!new\s+Error)(['"\d])/g,
    message: "Throw Error instances, not string or number literals",
  },
  {
    id: "no-this-alias",
    category: "correct",
    severity: "info",
    pattern: /(?:const|let|var)\s+(\w+)\s*=\s*this\s*;/g,
    message: "Unexpected aliasing of this to a local variable — use arrow functions instead",
  },
];
