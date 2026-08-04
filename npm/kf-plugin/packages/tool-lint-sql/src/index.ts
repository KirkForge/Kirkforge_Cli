import { LintEngine } from "@kirkforge/tool-lint-core";
import type { LintEngineOptions, LintReport } from "@kirkforge/tool-lint-core";
import type { LintRule } from "@kirkforge/tool-lint-core";

const SQL_EXTS = new Set([".sql"]);

const sqlRules: LintRule[] = [
  {
    id: "no-select-star",
    category: "perf",
    severity: "med",
    pattern: /\bSELECT\s+\*/gi,
    message: "SELECT * is inefficient; specify columns explicitly",
  },
  {
    id: "no-implicit-join",
    category: "correct",
    severity: "med",
    pattern: /\bFROM\s+\w+\s*,\s*\w+\s+WHERE\b/gi,
    message:
      "Implicit join (FROM a, b WHERE a.id = b.id) is error-prone — use explicit JOIN: FROM a INNER JOIN b ON a.id = b.id",
  },
  {
    id: "no-drop-table",
    category: "safety",
    severity: "critical",
    pattern: /\bDROP\s+TABLE\b/gi,
    message: "DROP TABLE is destructive; ensure this is intentional",
  },
  {
    id: "no-truncate",
    category: "safety",
    severity: "critical",
    pattern: /\bTRUNCATE\s+(?:TABLE\s+)?\w+/gi,
    message: "TRUNCATE removes all data without WHERE; ensure this is intentional",
  },
  {
    id: "no-unsafe-delete",
    category: "safety",
    severity: "high",
    pattern: /\bDELETE\s+FROM\s+\w+(?!.*\bWHERE\b)/gi,
    message: "DELETE without WHERE clause; add a WHERE condition or use TRUNCATE",
  },
  {
    id: "no-dynamic-injection",
    category: "safety",
    severity: "critical",
    pattern: /(['"])\s*(?:\|\||\+)\s*\w+\s*(?:\|\||\+)\s*['"]/g,
    message: "Potential SQL injection via string concatenation; use parameterized queries",
  },
];

export function createSqlLintEngine(opts: LintEngineOptions): LintEngine {
  const engine = new LintEngine({ ...opts, extensions: SQL_EXTS });
  engine.addRules(sqlRules);
  return engine;
}

export { LintEngine };
export type { LintEngineOptions, LintReport };
