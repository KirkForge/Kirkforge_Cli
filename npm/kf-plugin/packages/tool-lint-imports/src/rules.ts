import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { walkFiles } from "@kirkforge/core-logging";
import { ok, err } from "@kirkforge/core-types";
import type { Result, VerifierStatus } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import {
  BUILTIN_PYTHON_RENAMES,
  BUILTIN_TYPESCRIPT_RENAMES,
  type RenameEntry,
} from "./data.js";

export interface ImportFinding {
  file: string;
  line: number;
  rule: string;
  oldName: string;
  newName: string;
  reason: string;
  severity: "warning" | "info";
  sourceLanguage: "python" | "typescript";
}

export interface ImportReport {
  status: VerifierStatus;
  findings: number;
  warnings: number;
  info: number;
  filesScanned: number;
  durationMs: number;
  details: Array<{
    file: string;
    line: number;
    rule: string;
    oldName: string;
    newName: string;
    message: string;
  }>;
}

export interface ImportLintEngineOptions {
  cwd: string;
  eventBus?: EventBus;
  files?: string[];
  /** Renames to use. Defaults to the bundled Python + TypeScript tables. */
  pythonRenames?: Record<string, RenameEntry>;
  typescriptRenames?: Record<string, RenameEntry>;
  /** Languages to scan. Defaults to both. */
  languages?: Array<"python" | "typescript">;
}

// ── Import-pattern matchers ────────────────────────────────────────────────
//
// Python: `import X`, `import X.Y`, `from X import Y`, `from X.Y import Z`
//   The first identifier token after `import` (or after `from`) is the
//   top-level package name. We normalize and check against the renames table.
//
// TypeScript/JavaScript: `import X from 'Y'`, `import { a, b } from 'Y'`,
//   `import 'Y'`, `require('Y')`, dynamic `import('Y')`.
//   We extract the package name from the string literal.

const RE_PY_IMPORT = /^[ \t]*(?:import\s+([A-Za-z_][\w.]*)|from\s+([A-Za-z_][\w.]*)\s+import)/;
const RE_JS_IMPORT = /\b(?:from\s+|require\(\s*|import\(\s*)['"]([^'"]+)['"]/g;
const RE_JS_DYNAMIC = /import\(\s*['"]([^'"]+)['"]\s*\)/g;
const RE_JS_SIDE_EFFECT = /^[ \t]*import\s+['"]([^'"]+)['"]/;

function topLevelPythonPackage(name: string): string {
  return name.split(".")[0]!;
}

function topLevelJsPackage(specifier: string): string {
  // Strip leading protocol-like prefixes (e.g. "node:fs", "file://...")
  if (specifier.startsWith("node:")) return ""; // Node built-in
  if (specifier.startsWith("file:") || specifier.startsWith("./") ||
      specifier.startsWith("../") || specifier.startsWith("/")) {
    return ""; // relative or absolute path
  }
  // Scoped packages: @scope/name — preserve the scope
  if (specifier.startsWith("@")) {
    const parts = specifier.split("/");
    return parts.length >= 2 ? `${parts[0]}/${parts[1]}` : "";
  }
  return specifier.split("/")[0]!;
}

function scanPythonLine(
  line: string,
  renames: Record<string, RenameEntry>,
  findings: ImportFinding[],
  file: string,
  lineIdx: number,
): void {
  const m = RE_PY_IMPORT.exec(line);
  if (!m) return;
  const raw = (m[1] ?? m[2]) ?? "";
  const pkg = topLevelPythonPackage(raw);
  if (!pkg) return;
  const entry = renames[pkg];
  if (!entry) return;
  findings.push({
    file,
    line: lineIdx + 1,
    rule: "import-renamed",
    oldName: pkg,
    newName: entry.replacedBy,
    reason: entry.reason,
    severity: "warning",
    sourceLanguage: "python",
  });
}

function scanJsLine(
  line: string,
  renames: Record<string, RenameEntry>,
  findings: ImportFinding[],
  file: string,
  lineIdx: number,
): void {
  // Reset regex state for each line (these are stateful global regexes)
  RE_JS_IMPORT.lastIndex = 0;
  RE_JS_DYNAMIC.lastIndex = 0;
  RE_JS_SIDE_EFFECT.lastIndex = 0;

  const matches: string[] = [];
  for (const re of [RE_JS_IMPORT, RE_JS_DYNAMIC]) {
    let m: RegExpExecArray | null;
    while ((m = re.exec(line)) !== null) {
      matches.push(m[1]!);
    }
  }
  const sideEffect = RE_JS_SIDE_EFFECT.exec(line);
  if (sideEffect) matches.push(sideEffect[1]!);

  for (const spec of matches) {
    const pkg = topLevelJsPackage(spec);
    if (!pkg) continue;
    const entry = renames[pkg];
    if (!entry) continue;
    findings.push({
      file,
      line: lineIdx + 1,
      rule: "import-renamed",
      oldName: pkg,
      newName: entry.replacedBy,
      reason: entry.reason,
      severity: "warning",
      sourceLanguage: "typescript",
    });
  }
}

export class ImportLintEngine {
  private cwd: string;
  private eventBus?: EventBus;
  private files?: string[];
  private pythonRenames: Record<string, RenameEntry>;
  private typescriptRenames: Record<string, RenameEntry>;
  private languages: Array<"python" | "typescript">;

  constructor(opts: ImportLintEngineOptions) {
    this.cwd = resolve(opts.cwd);
    this.eventBus = opts.eventBus;
    this.files = opts.files;
    this.pythonRenames = opts.pythonRenames ?? BUILTIN_PYTHON_RENAMES;
    this.typescriptRenames = opts.typescriptRenames ?? BUILTIN_TYPESCRIPT_RENAMES;
    this.languages = opts.languages ?? ["python", "typescript"];
  }

  async emit(taskId: string): Promise<Result<ImportReport, Error>> {
    const startedAt = Date.now();
    try {
      const findings: ImportFinding[] = [];
      const includeFilter = (rel: string): boolean => {
        if (this.languages.includes("python") && rel.endsWith(".py")) return true;
        if (this.languages.includes("typescript")) {
          return /\.(?:[cm]?js|jsx|ts|tsx|mjs|cjs|mts|cts)$/.test(rel);
        }
        return false;
      };

      const allFiles: string[] = this.files
        ? this.files.filter((f) => includeFilter(f))
        : await walkFiles(this.cwd, includeFilter);

      for (const relPath of allFiles) {
        const filePath = resolve(this.cwd, relPath);
        let content: string;
        try {
          content = await readFile(filePath, "utf-8");
        } catch {
          continue;
        }
        const lines = content.split("\n");
        const isPython = relPath.endsWith(".py");
        for (let i = 0; i < lines.length; i++) {
          const line = lines[i]!;
          if (isPython) {
            scanPythonLine(line, this.pythonRenames, findings, relPath, i);
          } else {
            scanJsLine(line, this.typescriptRenames, findings, relPath, i);
          }
        }
      }

      const warnings = findings.filter((f) => f.severity === "warning").length;
      const info = findings.filter((f) => f.severity === "info").length;
      // The imports verifier is advisory — a deprecated import is not a build
      // failure. The reducer picks up the warning count via the findings field
      // and surfaces it through `ReducedStatePacket.verification.imports.warnings`.
      // The status stays "pass" so the slot never fail-closes the build.
      const status: VerifierStatus = "pass";

      const report: ImportReport = {
        status,
        findings: findings.length,
        warnings,
        info,
        filesScanned: allFiles.length,
        durationMs: Date.now() - startedAt,
        details: findings.map((f) => ({
          file: f.file,
          line: f.line,
          rule: f.rule,
          oldName: f.oldName,
          newName: f.newName,
          message: `${f.oldName} is deprecated; use ${f.newName}. ${f.reason}`,
        })),
      };

      await this.eventBus?.emit({
        kind: "verify.imports",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: report.status,
          findings: report.findings,
          warnings: report.warnings,
          info: report.info,
          filesScanned: report.filesScanned,
          durationMs: report.durationMs,
          details: report.details,
        },
        timestamp: new Date().toISOString(),
      });

      return ok(report);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const report: ImportReport = {
        status: "error",
        findings: 0,
        warnings: 0,
        info: 0,
        filesScanned: 0,
        durationMs: Date.now() - startedAt,
        details: [],
      };
      await this.eventBus?.emit({
        kind: "verify.imports",
        schemaVersion: "v3",
        sequence: Date.now(),
        streamId: taskId,
        taskId,
        value: {
          status: "error",
          error: message,
          findings: 0,
          warnings: 0,
          info: 0,
          filesScanned: 0,
          durationMs: report.durationMs,
          details: [],
        },
        timestamp: new Date().toISOString(),
      });
      return err(new Error(`kirkforge-imports: ${message}`));
    }
  }
}

export function createImportLintEngine(opts: ImportLintEngineOptions): ImportLintEngine {
  return new ImportLintEngine(opts);
}
