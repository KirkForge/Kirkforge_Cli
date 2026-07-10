import type { LintRule } from "@kirkforge/tool-lint-core";

export const maintainRules: LintRule[] = [
  {
    id: "no-todo-fixme",
    category: "maintain",
    severity: "info",
    pattern: /#\s*(?:TODO|FIXME|HACK|XXX)\b/gi,
    message: "TODO/FIXME comment; address or convert to a ticket",
  },
  {
    id: "no-commented-code",
    category: "maintain",
    severity: "low",
    pattern: /^\s*#\s*(?:def\s|class\s|import\s|from\s|if\s|for\s|while\s|print\s|with\s)/gm,
    message: "Commented-out code; remove if unused",
  },
  {
    id: "require-docstring",
    category: "maintain",
    severity: "low",
    pattern: /^[ \t]*def\s+\w+\([^)]*\)(?:\s*->\s*\w+)?\s*:\s*$(?!\n[ \t]*("""|'''))/gm,
    message: "Function missing docstring; add a description",
  },
];
