import type { LintRule } from "@kirkforge/tool-lint-core";

export const maintainRules: LintRule[] = [
  {
    id: "no-todo-fixme",
    category: "maintain",
    severity: "info",
    pattern: /\/\/\s*(?:TODO|FIXME|HACK|XXX)\b/gi,
    message: "TODO/FIXME comment; address or convert to a ticket",
  },
  {
    id: "no-dead-code",
    category: "maintain",
    severity: "info",
    pattern:
      /^\s*\/\/\s*(?:const|let|var|function|return|if|for|while|switch|try|class|export|import)\s+[\w\s,;(){}\[\]<>=!&|+\-*\/]{20,}$/gm,
    message: "Commented-out code; remove if unused",
  },
  {
    id: "require-jsdoc",
    category: "maintain",
    severity: "info",
    pattern: /^export\s+(?:async\s+)?function\s+\w+\s*\(/gm,
    message: "Exported function missing JSDoc comment; add documentation",
  },
  {
    id: "no-require-import",
    category: "maintain",
    severity: "info",
    pattern: /\brequire\s*\(\s*['"][^'"]+['"]\s*\)/g,
    message:
      "Use ESM import instead of require() — if dynamic, add a kirkforge-lint-disable comment",
  },
];
