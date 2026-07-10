import { readFile } from "node:fs/promises";
import { relative, resolve } from "node:path";
import { ok, err } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import { walkFiles } from "@kirkforge/core-logging";
import type { LintRule, LintFinding } from "./rules.js";
import { RuleRegistry } from "./rules.js";

const SCANNABLE_EXTS = new Set([".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"]);
const MAX_LINES = 200;

// ── Suppression directive patterns ──
const RE_DISABLE_LINE = /kirkforge-lint-disable-line\s*(?:([a-z0-9-,\s]+))?\s*$/;
const RE_DISABLE_NEXT = /kirkforge-lint-disable-next-line\s*(?:([a-z0-9-,\s]+))?\s*$/;
const RE_DISABLE_BLOCK = /kirkforge-lint-disable\s*(?:([a-z0-9-,\s]+))?\s*$/;
const RE_ENABLE_BLOCK = /kirkforge-lint-enable\s*(?:([a-z0-9-,\s]+))?\s*$/;

function parseRuleList(raw: string | undefined): Set<string> {
  if (!raw) return new Set(); // empty = all rules
  return new Set(
    raw
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean),
  );
}

function isSuppressed(
  ruleId: string,
  lineIdx: number,
  lines: string[],
  blockSuppressions: Map<number, Set<string>>,
): boolean {
  const line = lines[lineIdx]!;

  // kirkforge-lint-disable-line on the same line
  const lineMatch = RE_DISABLE_LINE.exec(line);
  if (lineMatch) {
    const rules = parseRuleList(lineMatch[1]);
    return rules.size === 0 || rules.has(ruleId);
  }

  // kirkforge-lint-disable-next-line on the previous line
  if (lineIdx > 0) {
    const prevLine = lines[lineIdx - 1]!;
    const nextMatch = RE_DISABLE_NEXT.exec(prevLine);
    if (nextMatch) {
      const rules = parseRuleList(nextMatch[1]);
      if (rules.size === 0 || rules.has(ruleId)) return true;
    }
  }

  // Block-level suppressions (disable without enable)
  for (const [startIdx, rules] of blockSuppressions) {
    if (lineIdx >= startIdx && (rules.size === 0 || rules.has(ruleId))) {
      return true;
    }
  }

  return false;
}

function computeBlockSuppressions(lines: string[]): Map<number, Set<string>> {
  const suppressions = new Map<number, Set<string>>();
  const stack: Array<{ idx: number; rules: Set<string> }> = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const disableMatch = RE_DISABLE_BLOCK.exec(line);
    if (disableMatch) {
      stack.push({ idx: i, rules: parseRuleList(disableMatch[1]) });
      continue;
    }
    const enableMatch = RE_ENABLE_BLOCK.exec(line);
    if (enableMatch && stack.length > 0) {
      const start = stack.pop()!;
      suppressions.set(start.idx, start.rules);
    }
  }
  // Unclosed blocks remain active for the rest of the file
  for (const entry of stack) {
    suppressions.set(entry.idx, entry.rules);
  }

  return suppressions;
}

function matchIgnorePattern(relPath: string, patterns: string[]): boolean {
  for (const pattern of patterns) {
    // Simple glob: * matches any sequence, ? matches single char
    const regex = new RegExp(
      "^" +
        pattern
          .replace(/[.+^${}()|[\]\\]/g, "\\$&")
          .replace(/\*/g, ".*")
          .replace(/\?/g, ".") +
        "$",
    );
    if (regex.test(relPath)) return true;
    // Also test as substring for directory patterns like "tests/"
    if (pattern.endsWith("/") && relPath.includes(pattern)) return true;
  }
  return false;
}

export interface LintEngineOptions {
  cwd: string;
  eventBus?: EventBus;
  files?: string[];
  extensions?: Set<string>;
  /** Glob patterns for files to skip (e.g. ["*.test.ts", "tests/"]) */
  ignorePatterns?: string[];
}

export interface LintReport {
  source: string;
  status: "pass" | "fail" | "skipped" | "error";
  errors: number;
  warnings: number;
  suppressed: number;
  filesScanned: number;
  durationMs: number;
  details: Array<{ file: string; line: number; rule: string; message: string }>;
}

export class LintEngine {
  private registry: RuleRegistry;
  private cwd: string;
  private eventBus?: EventBus;
  private files?: string[];
  private extensions: Set<string>;
  private ignorePatterns: string[];

  constructor(opts: LintEngineOptions) {
    this.registry = new RuleRegistry();
    this.cwd = resolve(opts.cwd);
    this.eventBus = opts.eventBus;
    this.files = opts.files;
    this.extensions = opts.extensions ?? SCANNABLE_EXTS;
    this.ignorePatterns = opts.ignorePatterns ?? [];
  }

  addRule(rule: LintRule): void {
    this.registry.addRule(rule);
  }

  addRules(rules: LintRule[]): void {
    this.registry.addRules(rules);
  }

  async emit(taskId: string): Promise<Result<LintReport, Error>> {
    const startedAt = Date.now();
    try {
      const rules = this.registry.getRules();
      const findings: LintFinding[] = [];
      let suppressedCount = 0;

      const includeFilter = (rel: string): boolean => {
        const ext = rel.slice(rel.lastIndexOf("."));
        return this.extensions.has(ext);
      };

      const allFiles: string[] = this.files
        ? this.files
            .filter((f) => this.extensions.has(f.slice(f.lastIndexOf("."))))
            .map((f) => relative(this.cwd, resolve(this.cwd, f)))
        : await walkFiles(this.cwd, includeFilter);

      for (const relPath of allFiles) {
        // Skip files matching ignore patterns
        if (matchIgnorePattern(relPath, this.ignorePatterns)) continue;

        const filePath = resolve(this.cwd, relPath);
        try {
          const content = await readFile(filePath, "utf-8");
          const lines = content.split("\n");

          // Pre-compute block-level suppressions for this file
          const blockSuppressions = computeBlockSuppressions(lines);

          // File-level checks
          const ext = relPath.slice(relPath.lastIndexOf("."));
          const isShell = [".sh", ".bash", ".zsh"].includes(ext);
          if (isShell && !content.startsWith("#!")) {
            findings.push({
              file: relPath,
              line: 1,
              rule: "require-shebang",
              category: "style",
              severity: "med",
              message: "Script missing shebang; add #!/bin/bash or similar",
            });
          }
          if (lines.length > MAX_LINES) {
            findings.push({
              file: relPath,
              line: MAX_LINES + 1,
              rule: "max-lines",
              category: "style",
              severity: "info",
              message: `File has ${lines.length} lines; maximum is ${MAX_LINES}`,
            });
          }

          for (const rule of rules) {
            const pattern = new RegExp(rule.pattern.source, rule.pattern.flags);
            for (let i = 0; i < lines.length; i++) {
              const line = lines[i]!;
              if (pattern.test(line)) {
                // Check suppression directives
                if (isSuppressed(rule.id, i, lines, blockSuppressions)) {
                  suppressedCount++;
                  continue;
                }
                findings.push({
                  file: relPath,
                  line: i + 1,
                  rule: rule.id,
                  category: rule.category,
                  severity: rule.severity,
                  message: rule.message,
                });
              }
            }
          }
        } catch {
          // Skip unreadable files
        }
      }

      const errorFindings = findings.filter(
        (f) => f.severity === "critical" || f.severity === "high" || f.severity === "med",
      );
      const warningFindings = findings.filter((f) => f.severity === "low" || f.severity === "info");

      const report: LintReport = {
        source: taskId,
        status: errorFindings.length > 0 ? "fail" : "pass",
        errors: errorFindings.length,
        warnings: warningFindings.length,
        suppressed: suppressedCount,
        filesScanned: allFiles.length,
        durationMs: Date.now() - startedAt,
        details: [...errorFindings, ...warningFindings].map((f) => ({
          file: f.file,
          line: f.line,
          rule: f.rule,
          message: f.message,
        })),
      };

      // Emit verify.lint
      await this.eventBus?.emit({
        kind: "verify.lint",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: report.status,
          errors: report.errors,
          warnings: report.warnings,
          suppressed: report.suppressed,
          filesScanned: report.filesScanned,
          durationMs: report.durationMs,
          details: report.details,
        },
        timestamp: new Date().toISOString(),
      });

      // Emit verify.security from safety-category findings
      const safetyFindings = findings.filter((f) => f.category === "safety");
      const secCritical = safetyFindings.filter((f) => f.severity === "critical").length;
      const secHigh = safetyFindings.filter((f) => f.severity === "high").length;
      const secStatus = secCritical > 0 || secHigh > 0 ? "fail" : "pass";

      await this.eventBus?.emit({
        kind: "verify.security",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: secStatus,
          findings: safetyFindings.length,
          critical: secCritical,
          high: secHigh,
          filesScanned: allFiles.length,
          durationMs: report.durationMs,
          details: safetyFindings.map((f) => ({
            file: f.file,
            line: f.line,
            rule: f.rule,
            severity: f.severity,
            message: f.message,
          })),
        },
        timestamp: new Date().toISOString(),
      });

      return ok(report);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const report: LintReport = {
        source: taskId,
        status: "error",
        errors: 1,
        warnings: 0,
        suppressed: 0,
        filesScanned: 0,
        durationMs: Date.now() - startedAt,
        details: [{ file: "<lint-engine>", line: 0, rule: "verifier-error", message }],
      };
      await this.eventBus?.emit({
        kind: "verify.lint",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: "error",
          error: message,
          errors: 1,
          warnings: 0,
          suppressed: 0,
          filesScanned: 0,
          durationMs: report.durationMs,
          details: report.details,
        },
        timestamp: new Date().toISOString(),
      });
      await this.eventBus?.emit({
        kind: "verify.security",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: "error",
          error: message,
          findings: 1,
          critical: 1,
          high: 0,
          filesScanned: 0,
          durationMs: report.durationMs,
          details: [],
        },
        timestamp: new Date().toISOString(),
      });
      return err(new Error(`kirkforge-lint: ${message}`));
    }
  }
}
