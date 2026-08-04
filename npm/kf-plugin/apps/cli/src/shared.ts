import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";
import { URL } from "node:url";
import { FileAdapter, MemoryStore, type MemoryAdapter } from "@kirkforge/memory-palace";
import { TenantRegistry } from "@kirkforge/core-tenancy";

export const VERSION: string = (() => {
  try {
    const pkg = JSON.parse(readFileSync(new URL("../../package.json", import.meta.url), "utf-8"));
    return pkg.version ?? "0.0.0";
  } catch {
    return "0.0.0";
  }
})();

export const ALL_MODES: string[] = [
  "hard-prompt",
  "schema-contract",
  "artifact",
  "task-decompose",
];

export function exitError(message: string, json?: boolean): never {
  if (json) {
    process.stdout.write(JSON.stringify({ error: message }) + "\n");
  } else {
    process.stderr.write(`Error: ${message}\n`);
  }
  process.exit(1);
}

export interface ResolveMemoryStoreOptions {
  workspace?: string;
  memory?: string;
  sqlite?: boolean;
  json?: boolean;
}

/**
 * Resolve a memory store + adapter pair from the common workspace-or-memory
 * CLI flags. Returns the wired MemoryStore and the underlying adapter (so
 * callers can `await adapter.persist()` after writes).
 */
export async function resolveMemoryStore(opts: ResolveMemoryStoreOptions): Promise<{
  store: MemoryStore;
  adapter: MemoryAdapter;
}> {
  if (!opts.workspace && !opts.memory) {
    throw new Error("either --workspace or --memory is required");
  }

  let memoryPath: string;
  if (opts.workspace) {
    if (!existsSync(resolve(opts.workspace))) {
      exitError(`Workspace directory does not exist: ${opts.workspace}`, opts.json);
    }
    const registry = new TenantRegistry();
    const tenant = registry.register(opts.workspace);
    memoryPath = registry.resolvePath(tenant.tenantId, "memory.db");
  } else {
    memoryPath = opts.memory!;
  }

  let adapter: MemoryAdapter;
  if (opts.sqlite) {
    const { SqliteAdapter } = await import("@kirkforge/memory-palace/sqlite-adapter");
    adapter = new SqliteAdapter(memoryPath);
  } else {
    adapter = new FileAdapter(memoryPath);
  }

  return { store: new MemoryStore(adapter), adapter };
}
