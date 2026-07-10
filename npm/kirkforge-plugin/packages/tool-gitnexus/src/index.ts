import { ok, err } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const execFileAsync = promisify(execFile);

async function git(args: string[], cwd: string): Promise<string> {
  try {
    const { stdout } = await execFileAsync("git", args, {
      cwd,
      timeout: 30000,
      maxBuffer: 10 * 1024 * 1024,
    });
    return stdout.trim();
  } catch {
    return "";
  }
}

async function gitRepoExists(cwd: string): Promise<boolean> {
  try {
    const { stdout } = await execFileAsync("git", ["rev-parse", "--git-dir"], {
      cwd,
      timeout: 5000,
    });
    return stdout.trim().length > 0;
  } catch {
    return false;
  }
}

async function countLines(filePath: string): Promise<number> {
  try {
    const content = await readFile(filePath, "utf8");
    if (content.length === 0) return 0;
    // Count newlines — last line without trailing newline still counts
    const lines = content.split("\n");
    return lines[lines.length - 1] === "" ? lines.length - 1 : lines.length;
  } catch {
    return 0;
  }
}

export interface GitnexusReport {
  taskId: string;
  filesChanged: number;
  paths: string[];
  insertions: number;
  deletions: number;
  durationMs: number;
}

export class GitnexusEmitter {
  private writtenFiles: string[];

  constructor(private opts: { cwd: string; eventBus?: EventBus; writtenFiles?: string[] }) {
    this.writtenFiles = opts.writtenFiles ?? [];
  }

  async emit(taskId: string): Promise<Result<GitnexusReport, Error>> {
    const start = Date.now();
    const { cwd, eventBus } = this.opts;
    const isRepo = await gitRepoExists(cwd);

    // ── Not a git repo: use writtenFiles as source of truth ──────────
    if (!isRepo) {
      const paths = [...this.writtenFiles];
      let insertions = 0;
      for (const rel of paths) {
        insertions += await countLines(join(cwd, rel));
      }
      const report: GitnexusReport = {
        taskId,
        filesChanged: paths.length,
        paths,
        insertions,
        deletions: 0,
        durationMs: Date.now() - start,
      };
      await eventBus?.emit({
        kind: "state.changes",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          filesChanged: paths.length,
          paths,
          insertions,
          deletions: 0,
          durationMs: report.durationMs,
          warning: "not a git repository — using artifact-written files as change source",
        },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    }

    // ── Git repo: collect tracked + untracked, merge writtenFiles ────
    try {
      const headOutput = await git(["rev-parse", "--verify", "HEAD"], cwd);
      const emptyTree = "4b825dc642cb6eb9a060e54bf899d153036e3e7a";
      const ref = headOutput.length > 0 ? "HEAD" : emptyTree;

      const trackedOutput = await git(["diff", ref, "--name-only"], cwd);
      const untrackedOutput = await git(["ls-files", "--others", "--exclude-standard"], cwd);

      const trackedPaths = trackedOutput.split("\n").filter(Boolean);
      const untrackedPaths = untrackedOutput.split("\n").filter(Boolean);

      // Merge writtenFiles so artifact-emitted files that aren't yet staged are included
      const writtenNotInGit = this.writtenFiles.filter(
        (wf) => !trackedPaths.includes(wf) && !untrackedPaths.includes(wf),
      );
      const allPaths = [...new Set([...trackedPaths, ...untrackedPaths, ...writtenNotInGit])];

      // Git shortstat covers tracked changes only
      let insertions = 0,
        deletions = 0;
      const stat = await git(["diff", ref, "--shortstat"], cwd);
      const im = stat.match(/(\d+)\s+insertions/);
      const dm = stat.match(/(\d+)\s+deletions/);
      if (im) insertions = parseInt(im[1]!);
      if (dm) deletions = parseInt(dm[1]!);

      // Estimate insertions for untracked files (git doesn't count these)
      for (const rel of untrackedPaths) {
        insertions += await countLines(join(cwd, rel));
      }
      // Also count lines for writtenFiles that git doesn't know about yet
      for (const rel of writtenNotInGit) {
        insertions += await countLines(join(cwd, rel));
      }

      const report: GitnexusReport = {
        taskId,
        filesChanged: allPaths.length,
        paths: allPaths,
        insertions,
        deletions,
        durationMs: Date.now() - start,
      };

      await eventBus?.emit({
        kind: "state.changes",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          filesChanged: allPaths.length,
          paths: allPaths,
          insertions,
          deletions,
          durationMs: report.durationMs,
        },
        timestamp: new Date().toISOString(),
      });

      return ok(report);
    } catch (e) {
      return err(new Error(`gitnexus: ${e instanceof Error ? e.message : String(e)}`));
    }
  }
}
