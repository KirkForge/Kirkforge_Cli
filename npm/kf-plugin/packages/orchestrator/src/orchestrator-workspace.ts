import { mkdtempSync, cpSync, rmSync, mkdirSync, writeFileSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { shouldExcludeFromTurnCopy } from "./workspace.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";
import type { TaskInput, OrchestratorResult } from "./types.js";

/**
 * Copy the baseline cwd into a fresh tmp dir, delegate the task
 * inside that copy, and stash the tmp dir on `s.activeTurnWorkspace`
 * so subsequent validators can use it. Preserves `s.cwd` across the
 * nested delegation.
 */
export async function runIsolatedTurn(
  s: OrchestratorInternals,
  delegateFn: (t: TaskInput) => Promise<OrchestratorResult>,
  task: TaskInput,
  taskId: string,
  originalCwd: string,
): Promise<OrchestratorResult> {
  const turnWorkspace = mkdtempSync(join(tmpdir(), "kirkforge-turn-"));
  try {
    cpSync(originalCwd, turnWorkspace, {
      recursive: true,
      dereference: false,
      filter: (src: string) => shouldExcludeFromTurnCopy(src, originalCwd.length),
    });
    const savedCwd = s.cwd;
    s.cwd = turnWorkspace;
    try {
      return await delegateFn({ ...task, taskId, suppressMemory: true });
    } finally {
      s.cwd = savedCwd;
    }
  } finally {
    // Keep workspace alive for task validators that run after delegation
    s.activeTurnWorkspace = turnWorkspace;
  }
}

/** Remove the current per-turn workspace (idempotent). */
export function cleanupTurnWorkspace(s: OrchestratorInternals): void {
  try {
    if (s.activeTurnWorkspace) {
      rmSync(s.activeTurnWorkspace, { recursive: true, force: true });
      s.activeTurnWorkspace = null;
    }
  } catch {
    /* best effort */
  }
}

/**
 * Build a validator workspace: copy baseline cwd into a tmp dir, then
 * overlay any emitted files on top. Returns the tmp dir path.
 */
export async function createIsolatedWorkspace(
  s: OrchestratorInternals,
  emittedFiles?: Array<{ path: string; content?: string }>,
  baselineDir?: string,
): Promise<string> {
  try {
    const tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-validator-"));

    if (emittedFiles && emittedFiles.length > 0) {
      const baseline = baselineDir ?? s.cwd;
      cpSync(baseline, tmpDir, {
        recursive: true,
        dereference: false,
        filter: (src: string) => shouldExcludeFromTurnCopy(src, baseline.length),
      });
      for (const f of emittedFiles) {
        const src = resolve(baselineDir ?? s.cwd, f.path);
        const dst = resolve(tmpDir, f.path);
        try {
          mkdirSync(dirname(dst), { recursive: true });
          if (f.content !== undefined) {
            writeFileSync(dst, f.content, "utf-8");
          } else {
            try {
              copyFileSync(src, dst);
            } catch {
              /* file may not exist — skip */
            }
          }
        } catch {
          /* best effort per file */
        }
      }
    } else {
      const baseline = baselineDir ?? s.cwd;
      cpSync(baseline, tmpDir, {
        recursive: true,
        dereference: false,
        filter: (src: string) => shouldExcludeFromTurnCopy(src, baseline.length),
      });
    }

    s.isolatedWorkspaceDirs.push(tmpDir);
    return tmpDir;
  } catch (e) {
    s.logger?.error(
      `[orchestrator] Failed to create isolated validator workspace: ${e instanceof Error ? e.message : String(e)}`,
    );
    throw e instanceof Error ? e : new Error(String(e));
  }
}

/** Remove all isolated validator workspaces created this run. */
export function cleanupIsolatedWorkspace(s: OrchestratorInternals): void {
  for (const dir of s.isolatedWorkspaceDirs) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
  s.isolatedWorkspaceDirs = [];
}

/** Remove all baseline snapshot dirs (called from gracefulShutdown). */
export function cleanupBaselineDirs(s: OrchestratorInternals): void {
  for (const dir of s.isolatedBaselineDirs) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
  s.isolatedBaselineDirs = [];
}

/**
 * Snapshot cwd into a tmp dir once per correction loop. Cached on the
 * instance so subsequent turns reuse the same frozen state.
 */
export function ensureBaselineSnapshot(s: OrchestratorInternals): string {
  if (s.baselineSnapshotDir) return s.baselineSnapshotDir;
  const snapshotDir = mkdtempSync(join(tmpdir(), "kirkforge-baseline-"));
  cpSync(s.cwd, snapshotDir, {
    recursive: true,
    dereference: false,
    filter: (src: string) => {
      try {
        return shouldExcludeFromTurnCopy(src, s.cwd.length);
      } catch {
        return true;
      }
    },
  });
  s.baselineSnapshotDir = snapshotDir;
  s.isolatedBaselineDirs.push(snapshotDir);
  s.logger?.info(`[orchestrator] Baseline snapshot created at ${snapshotDir}`);
  return snapshotDir;
}
