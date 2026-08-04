import { describe, it, expect } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { EventBus } from "@kirkforge/core-events";
import { ok } from "@kirkforge/core-types";
import type { VerifySecurityEvent } from "@kirkforge/core-types";
import { SecurityEmitter } from "../src/security-emitter.js";

// Gate (Task 8 sub-task 2): a real security emitter — obfuscated dangerous calls
// that the lint safety regex rules MISS (bracket-keyed access, string-concatenation
// shell exec, vm.*) must be flagged. The lint `no-eval` rule matches `\beval\s*\(`
// literally, so `window["eval"](...)` and `child_process["exec"](...)` evade it;
// `no-shell-exec` requires `${}` interpolation, so `child_process.exec('ls ' + x)`
// evades it. This emitter catches all three.

async function captureSecurity(files: string[]): Promise<VerifySecurityEvent> {
  const bus = new EventBus();
  let captured: VerifySecurityEvent | undefined;
  bus.on<VerifySecurityEvent>("verify.security", (e) => {
    captured = e;
    return Promise.resolve(ok(undefined));
  });
  const emitter = new SecurityEmitter({ cwd: process.cwd(), eventBus: bus, files });
  await emitter.emit("task-sec-1");
  if (!captured) throw new Error("verify.security was not emitted");
  return captured;
}

describe("SecurityEmitter", () => {
  it("flags bracket-keyed eval that the literal no-eval regex misses", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-bracket-eval-"));
    try {
      // The lint `no-eval` rule is /\beval\s*\(/g — this evades it (no literal `eval(`).
      writeFileSync(join(dir, "a.ts"), `const x = (window as any)["eval"]("alert(1)");\n`);
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBeGreaterThanOrEqual(1);
      expect(ev.value.critical).toBeGreaterThanOrEqual(1);
      expect(ev.value.details.some((d) => d.rule === "no-bracket-eval")).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags bracket-keyed child_process.exec that no-shell-exec misses", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-bracket-exec-"));
    try {
      // `no-shell-exec` requires ${} interpolation; bracket-keyed + static string evades it.
      writeFileSync(
        join(dir, "a.ts"),
        `import child_process from "child_process";\nchild_process["exec"]("rm -rf /tmp/x");\n`,
      );
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBeGreaterThanOrEqual(1);
      expect(ev.value.critical).toBeGreaterThanOrEqual(1);
      expect(ev.value.details.some((d) => d.rule === "no-bracket-shell-exec")).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags string-concatenation shell exec that the interpolation-only rule misses", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-concat-exec-"));
    try {
      // The lint `no-shell-exec` rule requires ${} interpolation; plain string
      // concatenation like "ls " + x evades it. `no-shell-exec-concat` catches the
      // qualified child_process.exec call regardless of argument shape.
      writeFileSync(
        join(dir, "a.ts"),
        `import child_process from "child_process";\nchild_process.exec("ls " + userInput);\n`,
      );
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBeGreaterThanOrEqual(1);
      expect(ev.value.details.some((d) => d.rule === "no-shell-exec-concat")).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags vm.runInNewContext code generation", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-vm-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import vm from "vm";\nvm.runInNewContext(untrusted);\n`);
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBeGreaterThanOrEqual(1);
      expect(ev.value.details.some((d) => d.rule === "no-vm-codegen")).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags Python eval / os.system / subprocess shell=True / pickle.loads", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-py-"));
    try {
      writeFileSync(
        join(dir, "a.py"),
        [
          `import os, subprocess, pickle`,
          `eval("1+1")`,
          `os.system("ls")`,
          `subprocess.run(["echo", x], shell=True)`,
          `pickle.loads(blob)`,
        ].join("\n") + "\n",
      );
      const ev = await captureSecurity([join(dir, "a.py")]);
      const rules = ev.value.details.map((d) => d.rule);
      expect(rules).toContain("py-eval");
      expect(rules).toContain("py-os-system");
      expect(rules).toContain("py-subprocess-shell");
      expect(rules).toContain("py-pickle-load");
      expect(ev.value.findings).toBeGreaterThanOrEqual(4);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("passes (zero findings) on clean code", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-clean-"));
    try {
      writeFileSync(join(dir, "a.ts"), `export const add = (a: number, b: number) => a + b;\n`);
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBe(0);
      expect(ev.value.status).toBe("pass");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("does not flag dangerous patterns inside comments", async () => {
    const dir = mkdtempSync(join(tmpdir(), "sec-comment-"));
    try {
      writeFileSync(
        join(dir, "a.ts"),
        `// don't use window["eval"] or child_process["exec"] here\nexport const x = 1;\n`,
      );
      const ev = await captureSecurity([join(dir, "a.ts")]);
      expect(ev.value.findings).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});