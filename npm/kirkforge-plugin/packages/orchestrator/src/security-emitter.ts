import { readFileSync, existsSync } from "node:fs";
import { extname } from "node:path";
import type { EventBus } from "@kirkforge/core-events";

// ponytail: token/regex dangerous-call scanner, not tree-sitter or semgrep/bandit.
// The lint safety rules (tool-lint-ts/rules/safety.ts) catch literal `eval(`,
// `new Function(`, and `${}`-interpolated shell exec, but MISS obfuscated forms —
// bracket-keyed access (`window["eval"]`, `child_process["exec"]`), string-
// concatenation shell exec (`child_process.exec('ls ' + x)`), and `vm.*` code
// generation. This emitter closes that gap. semgrep/bandit are the upgrade path
// when available; the built-in scanner keeps verification working with no external
// tool dependency.

interface Finding {
  file: string;
  line: number;
  rule: string;
  severity: "critical" | "high" | "medium" | "low";
  message: string;
}

interface ScanRule {
  rule: string;
  severity: Finding["severity"];
  pattern: RegExp;
  message: string;
  langs: ("js" | "py")[];
}

const RULES: ScanRule[] = [
  // --- Obfuscated eval (bracket-keyed) — the regex `no-eval` misses these ---
  {
    rule: "no-bracket-eval",
    severity: "critical",
    pattern: /\[\s*['"]eval['"]\s*\]\s*\(/g,
    message: "Bracket-keyed eval (e.g. window['eval']) — obfuscated arbitrary code execution; use JSON.parse or a sandboxed VM",
    langs: ["js"],
  },
  {
    rule: "no-bracket-function",
    severity: "high",
    pattern: /\[\s*['"]Function['"]\s*\]/g,
    message: "Bracket-keyed Function constructor — string-to-code compilation via evasion",
    langs: ["js"],
  },
  // --- Obfuscated shell exec (bracket-keyed) — no-shell-exec misses these ---
  {
    rule: "no-bracket-shell-exec",
    severity: "critical",
    pattern: /child_process\s*\[\s*['"](?:exec|execSync|spawn|spawnSync|fork)['"]\s*\]\s*\(/g,
    message: "Bracket-keyed child_process exec/spawn — obfuscated shell execution; use execFile with a static command + args array",
    langs: ["js"],
  },
  // --- child_process.exec with any call (the lint rule only flags ${} interpolation;
  //     string concatenation like 'ls ' + x evades it) ---
  {
    rule: "no-shell-exec-concat",
    severity: "high",
    pattern: /child_process\s*\.\s*(?:exec|execSync)\s*\(/g,
    message: "child_process.exec spawns a shell — use execFile with a static command and args array to prevent injection (string concatenation evades the interpolation-only lint rule)",
    langs: ["js"],
  },
  {
    rule: "no-required-shell-exec",
    severity: "high",
    pattern: /require\s*\(\s*['"]child_process['"]\s*\)\s*\.\s*(?:exec|execSync|spawn|spawnSync)\s*\(/g,
    message: "Inline-required child_process exec/spawn — use execFile with a static command and args array",
    langs: ["js"],
  },
  // --- vm code generation ---
  {
    rule: "no-vm-codegen",
    severity: "high",
    pattern: /\bvm\s*\.\s*(?:runInContext|runInNewContext|compileFunction)\s*\(/g,
    message: "vm.runIn*/compileFunction executes arbitrary code — avoid compiling untrusted strings",
    langs: ["js"],
  },
  {
    rule: "no-reflect-eval",
    severity: "critical",
    pattern: /Reflect\s*\.\s*(?:apply|construct)\s*\(\s*eval\b/g,
    message: "Reflect.apply/construct(eval) — aliased arbitrary code execution",
    langs: ["js"],
  },
  // --- Python ---
  {
    rule: "py-eval",
    severity: "critical",
    pattern: /\beval\s*\(/g,
    message: "Python eval() executes arbitrary code — use ast.literal_eval for data",
    langs: ["py"],
  },
  {
    rule: "py-exec",
    severity: "critical",
    pattern: /\bexec\s*\(/g,
    message: "Python exec() executes arbitrary code — restructure to avoid runtime code generation",
    langs: ["py"],
  },
  {
    rule: "py-os-system",
    severity: "high",
    pattern: /\bos\s*\.\s*(?:system|popen)\s*\(/g,
    message: "os.system/os.popen spawns a shell — use subprocess with a static arg list",
    langs: ["py"],
  },
  {
    rule: "py-subprocess-shell",
    severity: "high",
    pattern: /subprocess\s*\.\s*(?:Popen|call|run|check_output|check_call)\s*\([^)]*?shell\s*=\s*True/g,
    message: "subprocess with shell=True is shell injection — pass a static arg list with shell=False",
    langs: ["py"],
  },
  {
    rule: "py-builtin-eval-alias",
    severity: "critical",
    pattern: /(?:__builtins__\s*\[\s*['"]eval['"]|getattr\s*\(\s*__builtins__\s*,\s*['"]eval['"])/g,
    message: "Obfuscated Python eval via __builtins__ — arbitrary code execution",
    langs: ["py"],
  },
  {
    rule: "py-pickle-load",
    severity: "high",
    pattern: /\bpickle\s*\.\s*loads?\s*\(/g,
    message: "pickle.loads executes arbitrary code on untrusted input — use JSON or a safe format",
    langs: ["py"],
  },
  {
    rule: "py-yaml-load",
    severity: "high",
    pattern: /\byaml\s*\.\s*load\s*\(/g,
    message: "yaml.load is unsafe — use yaml.safe_load",
    langs: ["py"],
  },
];

function stripComments(src: string, isPy: boolean): string {
  // ponytail: strip comments only (NOT strings) — the obfuscated patterns are
  // string-keyed, so stripping strings would erase the very signal we scan for.
  let out = src.replace(/\/\*[\s\S]*?\*\//g, "");
  out = out.replace(/\/\/[^\n]*/g, "");
  if (isPy) out = out.replace(/^\s*#[^\n]*/gm, "");
  return out;
}

function lineOf(src: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index && i < src.length; i++) {
    if (src[i] === "\n") line++;
  }
  return line;
}

const JS_EXT_SET = new Set([".ts", ".tsx", ".mjs", ".cjs", ".js", ".jsx", ".mts", ".cts"]);

export class SecurityEmitter {
  constructor(private opts: { cwd?: string; eventBus: EventBus; files?: string[] }) {}

  async emit(taskId: string): Promise<void> {
    const start = Date.now();
    const files = (this.opts.files ?? []).filter((f) => existsSync(f));
    const findings: Finding[] = [];

    for (const file of files) {
      const ext = extname(file);
      const isJs = JS_EXT_SET.has(ext);
      const isPy = ext === ".py";
      if (!isJs && !isPy) continue;
      let src: string;
      try {
        src = readFileSync(file, "utf8");
      } catch {
        continue;
      }
      const clean = stripComments(src, isPy);
      const langs: ("js" | "py")[] = isJs ? ["js"] : ["py"];
      for (const rule of RULES) {
        if (!rule.langs.some((l) => langs.includes(l))) continue;
        rule.pattern.lastIndex = 0;
        let m: RegExpExecArray | null;
        while ((m = rule.pattern.exec(clean)) !== null) {
          findings.push({
            file,
            line: lineOf(clean, m.index),
            rule: rule.rule,
            severity: rule.severity,
            message: rule.message,
          });
        }
      }
    }

    const critical = findings.filter((f) => f.severity === "critical").length;
    const high = findings.filter((f) => f.severity === "high").length;

    await this.opts.eventBus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 0,
      streamId: taskId,
      taskId,
      value: {
        status: findings.length > 0 ? "fail" : "pass",
        findings: findings.length,
        critical,
        high,
        filesScanned: files.length,
        durationMs: Date.now() - start,
        details: findings,
      },
      timestamp: new Date().toISOString(),
    });
  }
}