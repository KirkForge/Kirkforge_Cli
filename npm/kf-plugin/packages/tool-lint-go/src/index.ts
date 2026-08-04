import { LintEngine } from "@kirkforge/tool-lint-core";
import type { LintEngineOptions, LintReport } from "@kirkforge/tool-lint-core";
import type { LintRule } from "@kirkforge/tool-lint-core";

const GO_EXTS = new Set([".go"]);

const goRules: LintRule[] = [
  {
    id: "no-naked-return",
    category: "style",
    severity: "med",
    pattern: /\breturn\s*$(?=\s*\S)/gm,
    message:
      "Naked return with named return values is surprising — explicitly list the return values for clarity, especially in functions longer than a few lines",
  },
  {
    id: "no-panic",
    category: "safety",
    severity: "high",
    pattern: /\bpanic\s*\(/g,
    message:
      "panic() crashes the program with a stack trace — return an error and let the caller decide how to handle it",
  },
  {
    id: "no-global-var",
    category: "style",
    severity: "med",
    pattern: /^var\s+\w+\s+[^(\n]+$(?!.*\bfunc\b)/gm,
    message:
      "Package-level mutable variable is shared global state — encapsulate in a struct, or use sync/atomic for concurrency safety",
  },
  {
    id: "no-unhandled-error",
    category: "correct",
    severity: "high",
    pattern: /\b\w+,\s*_\s*:?=\s*\w+/g,
    message:
      "Ignored error with _ — always check: if val, err := fn(); err != nil { return err }. In Go, every error must be handled",
  },
  {
    id: "no-defer-in-loop",
    category: "perf",
    severity: "med",
    pattern: /for\s+.*\{[\s\S]*?\bdefer\b/g,
    message:
      "defer in a loop defers until function return, not iteration end — wrap the loop body in a closure or extract to a function",
  },
  {
    id: "no-string-title",
    category: "style",
    severity: "low",
    pattern: /\bstrings\.Title\s*\(/g,
    message:
      "strings.Title is deprecated and removed in newer Go — use golang.org/x/text/cases for Unicode-aware title casing",
  },
  {
    id: "no-init-side-effect",
    category: "maintain",
    severity: "med",
    pattern: /^func\s+init\s*\(\s*\)\s*\{[\s\S]*?\b(?!return)/gm,
    message:
      "init() side effects make testing and dependency ordering hard — move initialization to an explicit Setup() or New() function",
  },
];

export function createGoLintEngine(opts: LintEngineOptions): LintEngine {
  const engine = new LintEngine({ ...opts, extensions: GO_EXTS });
  engine.addRules(goRules);
  return engine;
}

export { LintEngine };
export type { LintEngineOptions, LintReport };
