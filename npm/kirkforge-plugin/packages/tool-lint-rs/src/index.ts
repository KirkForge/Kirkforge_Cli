import { LintEngine } from "@kirkforge/tool-lint-core";
import type { LintEngineOptions, LintReport } from "@kirkforge/tool-lint-core";
import type { LintRule } from "@kirkforge/tool-lint-core";

const RS_EXTS = new Set([".rs"]);

const rustRules: LintRule[] = [
  {
    id: "no-unwrap",
    category: "safety",
    severity: "high",
    pattern: /\.unwrap\s*\(/g,
    message: "Avoid .unwrap(); use proper error handling with ? or match",
  },
  {
    id: "no-expect-in-prod",
    category: "safety",
    severity: "med",
    pattern: /\.expect\s*\(/g,
    message: "expect() panics on failure; use proper error handling in production",
  },
  {
    id: "no-unsafe",
    category: "safety",
    severity: "high",
    pattern: /\bunsafe\s*\{/g,
    message: "Unsafe block detected; minimize and document with SAFETY comment",
  },
  {
    id: "no-clone-on-copy",
    category: "perf",
    severity: "low",
    pattern: /\.clone\s*\(\)(?=\s*[;),\]])/g,
    message: "Unnecessary .clone() on Copy type; values copy implicitly",
  },
  {
    id: "no-println-in-lib",
    category: "style",
    severity: "med",
    pattern: /\bprintln!\s*\(/g,
    message: "println! in library code; use log crate instead",
  },
  {
    id: "no-todo",
    category: "maintain",
    severity: "info",
    pattern: /\btodo!\s*\(/g,
    message: "todo!() placeholder; implement or file a ticket",
  },
  {
    id: "no-dbg",
    category: "maintain",
    severity: "low",
    pattern: /\bdbg!\s*\(/g,
    message: "dbg!() left in code; remove before committing",
  },
  {
    id: "max-func-lines",
    category: "style",
    severity: "low",
    pattern: /^(\s*)fn\s.*\n(\1\{[\s\S]*?\n\1\})/gm,
    message: "Function may be too long; consider splitting",
  },
];

export function createRsLintEngine(opts: LintEngineOptions): LintEngine {
  const engine = new LintEngine({ ...opts, extensions: RS_EXTS });
  engine.addRules(rustRules);
  return engine;
}

export { LintEngine };
export type { LintEngineOptions, LintReport };
