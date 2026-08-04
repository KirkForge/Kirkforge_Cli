import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const PKG = join(ROOT, "..", "correction-core");

const FORBIDDEN_IMPORTS = [
  "@kirkforge/orchestrator",
  "@kirkforge/agent-core",
  "@kirkforge/model-config",
  "@kirkforge/model-client",
  "@kirkforge/plugin",
  "child_process",
  "node:child_process",
  "http",
  "node:http",
  "https",
  "node:https",
  "net",
  "node:net",
];

function collectSourceFiles(dir: string): string[] {
  const entries = readdirSync(dir, { withFileTypes: true });
  const files: string[] = [];
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory() && entry.name !== "dist") {
      files.push(...collectSourceFiles(full));
    } else if (entry.isFile() && entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
      files.push(full);
    }
  }
  return files;
}

describe("correction-core package boundary", () => {
  it("package.json has zero dependencies", () => {
    const pkg = JSON.parse(readFileSync(join(PKG, "package.json"), "utf-8"));
    const deps = Object.keys(pkg.dependencies ?? {});
    const devDeps = Object.keys(pkg.devDependencies ?? {});
    expect(deps.length).toBe(0);
    expect(devDeps.length).toBe(0);
  });

  it("source files do not import forbidden modules", () => {
    const files = collectSourceFiles(join(PKG, "src"));
    expect(files.length).toBeGreaterThan(0);

    for (const file of files) {
      const content = readFileSync(file, "utf-8");
      for (const forbidden of FORBIDDEN_IMPORTS) {
        expect(content, `${file} imports ${forbidden}`).not.toContain(`from "${forbidden}"`);
        expect(content, `${file} imports ${forbidden}`).not.toContain(`from '${forbidden}'`);
        if (forbidden.startsWith("@kirkforge/")) {
          expect(content, `${file} imports ${forbidden}`).not.toContain(`import("${forbidden}")`);
        }
      }
    }
  });

  it("does not import plugin or orchestrator even via require", () => {
    const files = collectSourceFiles(join(PKG, "src"));
    for (const file of files) {
      const content = readFileSync(file, "utf-8");
      expect(content, `${file} requires @kirkforge/plugin`).not.toContain(
        'require("@kirkforge/plugin")',
      );
      expect(content, `${file} requires @kirkforge/orchestrator`).not.toContain(
        'require("@kirkforge/orchestrator")',
      );
    }
  });

  it("plugin may import correction-core (not forbidden)", () => {
    const pluginCoreSrc = join(ROOT, "..", "plugin", "src");
    const files = collectSourceFiles(pluginCoreSrc);
    const hasCorrectionCoreImport = files.some((f) => {
      const content = readFileSync(f, "utf-8");
      return content.includes("@kirkforge/correction-core");
    });
    expect(hasCorrectionCoreImport, "plugin should import correction-core").toBe(true);
  });

  it("orchestrator may import correction-core (not forbidden)", () => {
    const orchestratorSrc = join(ROOT, "..", "orchestrator", "src");
    const files = collectSourceFiles(orchestratorSrc);
    const hasCorrectionCoreImport = files.some((f) => {
      const content = readFileSync(f, "utf-8");
      return content.includes("@kirkforge/correction-core");
    });
    expect(hasCorrectionCoreImport, "orchestrator should import correction-core").toBe(true);
  });
});
