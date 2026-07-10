import { readdir } from "node:fs/promises";
import { relative, resolve } from "node:path";

/**
 * Recursively walk a directory, returning relative paths of files
 * that match the include predicate. Excludes common noise directories.
 */
export async function walkFiles(
  cwd: string,
  include: (relativePath: string) => boolean,
): Promise<string[]> {
  const results: string[] = [];
  async function visit(dir: string): Promise<void> {
    let entries: Array<{ name: string; isDirectory(): boolean; isFile(): boolean }>;
    try {
      entries = (await readdir(dir, { withFileTypes: true })) as Array<{
        name: string;
        isDirectory(): boolean;
        isFile(): boolean;
      }>;
    } catch {
      return;
    }
    for (const entry of entries) {
      if (
        entry.name === "node_modules" ||
        entry.name === ".git" ||
        entry.name === "dist" ||
        entry.name === ".tsbuildinfo"
      )
        continue;
      const full = resolve(dir, entry.name);
      const rel = relative(cwd, full);
      if (entry.isDirectory()) await visit(full);
      else if (entry.isFile() && include(rel)) results.push(rel);
    }
  }
  await visit(cwd);
  return results;
}
