import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { createVerificationEmitters } from "../src/emitter-factory.js";
import { mkdtempSync, writeFileSync, mkdirSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { execSync } from "child_process";

describe("ChangesEmitter", () => {
  it("emits real insertions/deletions from git diff", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kf-changes-"));
    const subdir = join(dir, "src");
    mkdirSync(subdir);
    const file = join(subdir, "index.ts");

    execSync("git init", { cwd: dir, stdio: "ignore" });
    execSync("git config user.email test@x && git config user.name test", { cwd: dir, stdio: "ignore" });
    writeFileSync(file, "const a = 1;\n");
    execSync("git add . && git commit -m init", { cwd: dir, stdio: "ignore" });
    writeFileSync(file, "const a = 1;\nconst b = 2;\nconst c = 3;\n");

    const bus = new EventBus();
    const emitters = createVerificationEmitters(dir, bus, undefined, undefined, ["src/index.ts"]);
    const events: any[] = [];
    bus.on("state.changes", async (evt) => { events.push(evt); return { ok: true, value: undefined }; });

    await emitters.changes.emit("task-1");

    expect(events.length).toBe(1);
    const value = events[0]!.value;
    expect(value.filesChanged).toBe(1);
    expect(value.insertions).toBe(2);
    expect(value.deletions).toBe(0);
    expect(value.paths).toEqual(["src/index.ts"]);
  });

  it("falls back to file count outside a git repo", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kf-changes-nogit-"));
    const bus = new EventBus();
    const emitters = createVerificationEmitters(dir, bus, undefined, undefined, ["a.ts"]);
    const events: any[] = [];
    bus.on("state.changes", async (evt) => { events.push(evt); return { ok: true, value: undefined }; });

    await emitters.changes.emit("task-2");

    expect(events.length).toBe(1);
    const value = events[0]!.value;
    expect(value.filesChanged).toBe(1);
    expect(value.insertions).toBe(0);
    expect(value.deletions).toBe(0);
  });
});
