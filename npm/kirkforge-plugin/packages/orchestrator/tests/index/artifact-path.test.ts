import { describe, it, expect } from "vitest";
import { mkdtempSync, readFileSync, existsSync, rmSync, mkdirSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArtifacts, writeArtifacts } from "../../src/artifact-mode.js";
import { detectTaskProfile } from "../../src/task-profile.js";

describe("artifact path and extension enforcement", () => {
  const pythonProfile = detectTaskProfile("write a python script");

  it("blocks python task emitting .ts file", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-ext-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: output.ts\nconsole.log('hi')\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("forbidden extension .ts");
      expect(existsSync(join(cwd, "output.ts"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks no-dot filenames like Dockerfile under profile that does not allow empty extension", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-nodot-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: Dockerfile\nFROM ubuntu\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("no-extension files not allowed");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("allows no-dot filenames like Dockerfile when no profile is set", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-nodot-noprof-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: Dockerfile\nFROM ubuntu\n### END");
      const results = writeArtifacts(artifacts, cwd);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(true);
      expect(results[0].filePath).toBe("Dockerfile");
      expect(readFileSync(join(cwd, "Dockerfile"), "utf-8")).toBe("FROM ubuntu\n");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("does not give Makefile a fake extension", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-makefile-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: Makefile\nall:\n\techo hi\n### END");
      const results = writeArtifacts(artifacts, cwd);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(true);
      expect(readFileSync(join(cwd, "Makefile"), "utf-8")).toBe("all:\n\techo hi\n");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks ../escape.py path escape", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-escape-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: ../escape.py\nprint('nope')\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("path escapes sandbox");
      expect(existsSync(join(cwd, "..", "escape.py"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks absolute path", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-abs-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: /etc/passwd\nroot:x:0:0\n### END");
      const results = writeArtifacts(artifacts, cwd);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("path escapes sandbox");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks sibling-prefix path escape", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-sibling-"));
    const siblingPath = cwd + "-evil" + "/file.py";
    try {
      const { artifacts } = parseArtifacts(`### FILE: ${siblingPath}\nprint('nope')\n### END`);
      const results = writeArtifacts(artifacts, cwd);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("path escapes sandbox");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("allows valid .py file for python profile", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-valid-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: solution.py\nprint('hello')\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(true);
      expect(readFileSync(join(cwd, "solution.py"), "utf-8")).toBe("print('hello')\n");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks .d.ts extension via python forbidden list (extracts .ts from last dot)", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-dts-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: types.d.ts\ndeclare module 'x'\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("forbidden extension .ts");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks hidden dotfile .env", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-dotenv-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: .env\nSECRET=123\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("hidden dotfile");
      expect(results[0].blocked).toContain(".env");
      expect(existsSync(join(cwd, ".env"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks hidden dotfile . gitignore", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-gitignore-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: .gitignore\nnode_modules\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("hidden dotfile");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks .npmrc dotfile", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-npmrc-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: .npmrc\nregistry=evil\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("hidden dotfile");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks long unknown extension that bypasses allowed list", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-longext-"));
    try {
      const { artifacts } = parseArtifacts(
        "### FILE: payload.notallowedbutlong\nprint('nope')\n### END",
      );
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("unexpected extension");
      expect(existsSync(join(cwd, "payload.notallowedbutlong"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("allows valid .py file for Python profile (still works after hardening)", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-py-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: solution.py\nprint('hello')\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(true);
      expect(readFileSync(join(cwd, "solution.py"), "utf-8")).toBe("print('hello')\n");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks .env even without a profile", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-dotenv-noprof-"));
    try {
      const { artifacts } = parseArtifacts("### FILE: .env\nSECRET=123\n### END");
      const results = writeArtifacts(artifacts, cwd);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("hidden dotfile");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks symlink escape when parent dir is a symlink pointing outside cwd", () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-symlink-"));
    const outsideDir = mkdtempSync(join(tmpdir(), "kirkforge-artifact-outside-"));
    try {
      mkdirSync(join(cwd, "src"), { recursive: true });
      symlinkSync(outsideDir, join(cwd, "src", "escape"));
      const { artifacts } = parseArtifacts("### FILE: src/escape/pwned.py\nprint('nope')\n### END");
      const results = writeArtifacts(artifacts, cwd, pythonProfile);
      expect(results).toHaveLength(1);
      expect(results[0].ok).toBe(false);
      expect(results[0].blocked).toContain("symlink escape detected");
      expect(existsSync(join(outsideDir, "pwned.py"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
      rmSync(outsideDir, { recursive: true, force: true });
    }
  });
});
