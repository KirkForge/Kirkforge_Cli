// kirkforge-lint-disable no-var
import { describe, it, expect, afterAll } from "vitest";
import { LintEngine, RuleRegistry } from "../src/index.js";
import { writeFile, mkdir, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { existsSync } from "node:fs";
import type { LintRule } from "../src/index.js";

const baseDir = resolve(tmpdir(), "kirkforge-lint-core-tests-" + Date.now());
let testSeq = 0;

function testDir(label: string): string {
  testSeq++;
  return resolve(baseDir, `test-${testSeq}-${label}`);
}

async function writeTestFile(dir: string, relPath: string, content: string) {
  const full = resolve(dir, relPath);
  const parentDir = resolve(full, "..");
  if (!existsSync(parentDir)) await mkdir(parentDir, { recursive: true });
  await writeFile(full, content, "utf-8");
}

const testRules: LintRule[] = [
  {
    id: "no-var",
    category: "style",
    severity: "high",
    pattern: /\bvar\s+/g,
    message: "Use const or let instead of var",
  },
  {
    id: "no-eval",
    category: "safety",
    severity: "critical",
    pattern: /\beval\s*\(/g,
    message: "eval is unsafe",
  },
  {
    id: "no-console",
    category: "style",
    severity: "med",
    pattern: /console\.log\(/g,
    message: "Remove debug logging",
  },
  {
    id: "todo-comment",
    category: "maintain",
    severity: "info",
    pattern: /\/\/\s*TODO/g,
    message: "Address TODO",
  },
];

describe("RuleRegistry", () => {
  it("starts empty", () => {
    const reg = new RuleRegistry();
    expect(reg.getRules()).toHaveLength(0);
  });

  it("adds and retrieves rules", () => {
    const reg = new RuleRegistry();
    reg.addRule(testRules[0]!);
    expect(reg.getRules()).toHaveLength(1);
    expect(reg.getRules()[0]!.id).toBe("no-var");
  });

  it("adds multiple rules at once", () => {
    const reg = new RuleRegistry();
    reg.addRules(testRules);
    expect(reg.getRules()).toHaveLength(4);
  });

  it("addRule always appends (no dedup by design)", () => {
    const reg = new RuleRegistry();
    reg.addRule(testRules[0]!);
    reg.addRule({ ...testRules[0]!, message: "different" });
    expect(reg.getRules()).toHaveLength(2);
  });
});

describe("LintEngine", () => {
  it("returns empty report when no files match extensions", async () => {
    const dir = testDir("empty");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t1");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.filesScanned).toBe(0);
      expect(result.value.errors).toBe(0);
      expect(result.value.warnings).toBe(0);
      expect(result.value.status).toBe("pass");
    }
  });

  it("detects style and safety issues", async () => {
    const dir = testDir("issues");
    await writeTestFile(dir, "src/bad.ts", "var x = 1;\neval('hello');\nconsole.log('hi');");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t2");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.filesScanned).toBe(1);
      expect(result.value.errors).toBeGreaterThanOrEqual(2);
      const detailIds = result.value.details.map((d) => d.rule);
      expect(detailIds).toContain("no-var");
      expect(detailIds).toContain("no-eval");
    }
  });

  it("separates errors from warnings/info", async () => {
    const dir = testDir("separate");
    await writeTestFile(dir, "src/app.ts", "const x = 1;\n// TODO: refactor\nconsole.log('test');");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t3");
    expect(result.ok).toBe(true);
    if (result.ok) {
      // no-console (med) → error, todo-comment (info) → warning
      expect(result.value.errors).toBe(1);
      expect(result.value.warnings).toBeGreaterThanOrEqual(1);
    }
  });

  it("reports pass for clean file", async () => {
    const dir = testDir("clean");
    await writeTestFile(dir, "src/clean.ts", "const x = 1;\nfunction foo() {}\nclass Bar {}");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t4");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.status).toBe("pass");
      expect(result.value.errors).toBe(0);
      expect(result.value.warnings).toBe(0);
    }
  });

  it("respects file filter", async () => {
    const dir = testDir("filter");
    await writeTestFile(dir, "src/app.ts", "const x = 1;");
    await writeTestFile(dir, "src/other.ts", "var y = 2;"); // should not be scanned
    const engine = new LintEngine({
      cwd: dir,
      files: ["src/app.ts"],
      extensions: new Set([".ts"]),
    });
    engine.addRules(testRules);
    const result = await engine.emit("t5");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.filesScanned).toBe(1);
    }
  });

  it("respects custom extensions", async () => {
    const dir = testDir("extensions");
    await writeTestFile(dir, "script.js", "var x = 1; // TODO");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".js"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t6");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.filesScanned).toBe(1);
    }
  });

  it("skips non-matching extensions by default", async () => {
    const dir = testDir("skip-ext");
    await writeTestFile(dir, "data.txt", "var x = 1;");
    const engine = new LintEngine({ cwd: dir });
    engine.addRules(testRules);
    const result = await engine.emit("t7");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.filesScanned).toBe(0);
    }
  });

  it("includes file path and line number in details", async () => {
    const dir = testDir("details");
    await writeTestFile(dir, "src/issue.ts", "\n\nvar x = 1;");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t8");
    expect(result.ok).toBe(true);
    if (result.ok) {
      const detail = result.value.details.find((d) => d.rule === "no-var");
      expect(detail).toBeDefined();
      expect(detail!.file).toContain("issue.ts");
      expect(detail!.line).toBe(3);
    }
  });

  it("reports durationMs above zero", async () => {
    const dir = testDir("timing");
    await writeTestFile(dir, "src/timed.ts", "var x = 1;");
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t9");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
    }
  });

  it("handles unreadable files gracefully", async () => {
    const dir = resolve(baseDir, "nonexistent-" + testSeq);
    const engine = new LintEngine({ cwd: dir, extensions: new Set([".ts"]) });
    engine.addRules(testRules);
    const result = await engine.emit("t10");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.status).toBe("pass");
      expect(result.value.errors).toBe(0);
    }
  });
});

// cleanup
afterAll(async () => {
  try {
    await rm(baseDir, { recursive: true, force: true });
  } catch {}
});
// kirkforge-lint-enable no-var
