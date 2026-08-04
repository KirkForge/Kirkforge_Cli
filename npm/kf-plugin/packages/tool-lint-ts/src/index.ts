import { LintEngine } from "@kirkforge/tool-lint-core";
import type { LintEngineOptions, LintReport } from "@kirkforge/tool-lint-core";
import { styleRules } from "./rules/style.js";
import { correctRules } from "./rules/correct.js";
import { safetyRules } from "./rules/safety.js";
import { perfRules } from "./rules/perf.js";
import { maintainRules } from "./rules/maintain.js";

const TS_EXTS = new Set([".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"]);

export function createTSLintEngine(opts: LintEngineOptions): LintEngine {
  const engine = new LintEngine({ ...opts, extensions: TS_EXTS });
  engine.addRules(styleRules);
  engine.addRules(correctRules);
  engine.addRules(safetyRules);
  engine.addRules(perfRules);
  engine.addRules(maintainRules);
  return engine;
}

export { LintEngine };
export type { LintEngineOptions, LintReport };
export { styleRules, correctRules, safetyRules, perfRules, maintainRules };
