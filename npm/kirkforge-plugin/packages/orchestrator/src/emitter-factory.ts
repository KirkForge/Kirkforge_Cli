import { createTSLintEngine } from "@kirkforge/tool-lint-ts";
import { createPyLintEngine } from "@kirkforge/tool-lint-py";
import { createShLintEngine } from "@kirkforge/tool-lint-sh";
import { createCLintEngine } from "@kirkforge/tool-lint-c";
import { createRsLintEngine } from "@kirkforge/tool-lint-rs";
import { createGoLintEngine } from "@kirkforge/tool-lint-go";
import { createSqlLintEngine } from "@kirkforge/tool-lint-sql";
import { createImportLintEngine } from "@kirkforge/tool-lint-imports";
import { TscEmitter } from "@kirkforge/tool-tsc";
import { PyrightEmitter } from "@kirkforge/tool-pyright";
import type { EventBus } from "@kirkforge/core-events";
import type { TaskLanguage } from "./task-profile.js";
import { GraphEmitter } from "./graph-emitter.js";
import { SecurityEmitter } from "./security-emitter.js";
import { spawn } from "child_process";

function hasJsTs(files?: string[]): boolean {
  return (files ?? []).some((file) => /\.(?:[cm]?js|jsx|ts|tsx)$/.test(file));
}

// ponytail: the changes/graph/security verifier slots are emitted by in-repo
// emitters, not external tools. `ChangesEmitter` computes real insertions/deletions
// with `git diff --numstat -- <paths>` when git is available; it falls back to
// counting the number of written files with zero insertions/deletions if the
// working tree is not a git repo. `graph` is implemented in `graph-emitter.ts`
// (regex import-edge extraction → cycles/brokenEdges/newEdges); `security` is
// implemented in `security-emitter.ts` (obfuscated dangerous-call scan).
class ChangesEmitter {
  constructor(private opts: { eventBus: EventBus; writtenFiles?: string[]; cwd?: string }) {}

  private _gitDiffNumstat(paths: string[]): Promise<{ insertions: number; deletions: number; filesChanged: number }> {
    return new Promise((resolve) => {
      if (paths.length === 0) {
        resolve({ insertions: 0, deletions: 0, filesChanged: 0 });
        return;
      }
      const args = ["diff", "--numstat", "--"].concat(paths);
      const child = spawn("git", args, {
        stdio: ["ignore", "pipe", "ignore"],
        cwd: this.opts.cwd,
      });
      let stdout = "";
      child.stdout?.on("data", (chunk: Buffer) => {
        stdout += chunk.toString("utf8");
      });
      child.on("error", () => {
        resolve({ insertions: 0, deletions: 0, filesChanged: paths.length });
      });
        child.on("close", (code) => {
        if (code !== 0) {
          resolve({ insertions: 0, deletions: 0, filesChanged: paths.length });
          return;
        }
        const lines = stdout.trim().split("\n").filter((line) => line.length > 0);
        let insertions = 0;
        let deletions = 0;
        let filesChanged = 0;
        for (const line of lines) {
          const parts = line.split("\t");
          if (parts.length < 3) continue;
          const ins = parseInt(parts[0]!, 10);
          const dels = parseInt(parts[1]!, 10);
          if (!Number.isNaN(ins)) insertions += ins;
          if (!Number.isNaN(dels)) deletions += dels;
          filesChanged += 1;
        }
        // If git exits 0 but produces no stats (e.g. file not tracked), fall
        // back to counting the requested paths so the caller still sees the
        // files the tool reported writing.
        if (filesChanged === 0) {
          filesChanged = paths.length;
        }
        resolve({ insertions, deletions, filesChanged });
      });
    });
  }

  async emit(taskId: string) {
    const paths = this.opts.writtenFiles ?? [];
    const start = Date.now();
    const { insertions, deletions, filesChanged } = await this._gitDiffNumstat(paths);
    await this.opts.eventBus.emit({
      kind: "state.changes",
      schemaVersion: "v3",
      sequence: 0,
      streamId: taskId,
      taskId,
      value: {
        filesChanged,
        paths,
        insertions,
        deletions,
        durationMs: Date.now() - start,
      },
      timestamp: new Date().toISOString(),
    });
  }
}

export function createVerificationEmitters(
  cwd: string,
  eventBus: EventBus,
  files?: string[],
  language?: TaskLanguage,
  writtenFiles?: string[],
) {
  const pythonOnly = language === "python" || (!language && !hasJsTs(files));

  // Phase 1+2+3: KirkForge native strict lint for all supported languages
  const tsLint = createTSLintEngine({ cwd, eventBus, files });
  const pyLint = createPyLintEngine({ cwd, eventBus, files });

  // Phase 3: Language-specific native lint
  const lintByLang: Record<string, { emit: (taskId: string) => ReturnType<typeof tsLint.emit> }> = {
    shell: createShLintEngine({ cwd, eventBus, files }),
    c: createCLintEngine({ cwd, eventBus, files }),
    cpp: createCLintEngine({ cwd, eventBus, files }),
    rust: createRsLintEngine({ cwd, eventBus, files }),
    go: createGoLintEngine({ cwd, eventBus, files }),
    sql: createSqlLintEngine({ cwd, eventBus, files }),
    text: tsLint, // fallback to TS for text
  };

  const resolvedLint =
    language && lintByLang[language] ? lintByLang[language]! : pythonOnly ? pyLint : tsLint;
  // Real security emitter: obfuscated dangerous-call scan (bracket-keyed eval/exec,
  // string-concat shell exec, vm.*, py eval/os.system/subprocess shell=True/pickle).
  // The lint safety rules still run in the `lint` slot and catch the literal forms.
  const security = new SecurityEmitter({ cwd, eventBus, files });

  // Imports verifier: runs on both Python and TypeScript workspaces. Emits
  // verify.imports as an advisory slot by default — the reducer treats it
  // as a warning source, not a fail-closed hard fail.
  const imports = createImportLintEngine({ cwd, eventBus, files });

  return {
    lint: resolvedLint,
    types: pythonOnly
      ? new PyrightEmitter({ cwd, eventBus, files })
      : new TscEmitter({ cwd, eventBus, files }),
    security,
    changes: new ChangesEmitter({ eventBus, writtenFiles, cwd }),
    graph: new GraphEmitter({ eventBus, files, writtenFiles }),
    imports,
  };
}
