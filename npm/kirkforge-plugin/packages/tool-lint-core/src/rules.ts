export type Severity = "critical" | "high" | "med" | "low" | "info";

export interface LintRule {
  id: string;
  category: "style" | "correct" | "safety" | "perf" | "maintain";
  severity: Severity;
  pattern: RegExp;
  message: string;
}

export interface LintFinding {
  file: string;
  line: number;
  rule: string;
  category: string;
  severity: Severity;
  message: string;
}

export interface LintResult {
  source: string;
  status: "pass" | "error";
  errors: number;
  warnings: number;
  filesScanned: number;
  durationMs: number;
  details: LintFinding[];
}

export class RuleRegistry {
  private rules: LintRule[] = [];

  addRule(rule: LintRule): void {
    this.rules.push(rule);
  }

  addRules(rules: LintRule[]): void {
    this.rules.push(...rules);
  }

  getRules(): LintRule[] {
    return this.rules;
  }

  filterBySeverity(minSeverity: Severity): LintRule[] {
    const order: Severity[] = ["critical", "high", "med", "low", "info"];
    const minIdx = order.indexOf(minSeverity);
    return this.rules.filter((r) => order.indexOf(r.severity) >= minIdx);
  }
}
