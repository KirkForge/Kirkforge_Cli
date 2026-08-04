import type { LintRule } from "@kirkforge/tool-lint-core";

export const styleRules: LintRule[] = [
  {
    id: "no-bare-except",
    category: "style",
    severity: "high",
    pattern: /^[ \t]*except[ \t]*:[ \t]*(?:#.*)?$/gm,
    message: "Bare except clause; specify exception types to catch",
  },
  {
    id: "no-mutable-defaults",
    category: "style",
    severity: "high",
    pattern: /def \w+\([^)]*=\s*\[\s*\]|[^)]*=\s*\{\s*\}/g,
    message: "Mutable default argument; use None and set default in function body",
  },
  {
    id: "no-print",
    category: "style",
    severity: "med",
    pattern: /^[ \t]*print\s*\(/gm,
    message: "Use a proper logger instead of print()",
  },
  {
    id: "no-tabs",
    category: "style",
    severity: "low",
    pattern: /\t/g,
    message: "Use spaces instead of tabs",
  },
  {
    id: "no-trailing-whitespace",
    category: "style",
    severity: "low",
    pattern: /[ \t]+$/gm,
    message: "Trailing whitespace; trim your lines",
  },
  {
    id: "max-params",
    category: "style",
    severity: "low",
    pattern: /def \w+\([^)]*,[^)]*,[^)]*,[^)]*,[^)]*\)/g,
    message: "Function has too many parameters; consider a dataclass or options dict",
  },
  {
    id: "no-wildcard-import",
    category: "style",
    severity: "med",
    pattern: /from\s+\w+\s+import\s+\*/g,
    message: "Wildcard import; import only what you need",
  },
  {
    id: "prefer-pathlib",
    category: "style",
    severity: "low",
    pattern: /\bos\.path\./g,
    message: "Prefer pathlib over os.path for path manipulation",
  },
];
