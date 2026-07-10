import { ok } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { relative, resolve, dirname } from "node:path";

interface EdgeInfo {
  from: string;
  to: string;
  kind: "import" | "type-only" | "dynamic";
}

export interface GraphifyReport {
  taskId: string;
  edgeCount: number;
  newEdges: number;
  brokenEdges: number;
  cycles: number;
  durationMs: number;
}

const RESOLVABLE_EXTENSIONS = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"];

function tryResolveExtension(basePath: string): string | null {
  for (const ext of RESOLVABLE_EXTENSIONS) {
    if (existsSync(basePath + ext)) return basePath + ext;
  }
  for (const ext of [".ts", ".tsx", ".js"]) {
    if (existsSync(resolve(dirname(basePath), "index" + ext)))
      return resolve(dirname(basePath), "index" + ext);
  }
  return null;
}

export class GraphifyEmitter {
  constructor(private opts: { cwd: string; eventBus?: EventBus; files?: string[] }) {}

  async emit(taskId: string): Promise<Result<GraphifyReport, Error>> {
    const start = Date.now();
    const { cwd, eventBus, files } = this.opts;
    try {
      const targets = (files ?? []).filter((f) => /\.(?:ts|tsx|js|jsx|mjs|cjs|mts|cts)$/.test(f));
      const edges: EdgeInfo[] = [];
      const seen = new Set<string>();

      if (targets.length === 0) {
        const report: GraphifyReport = {
          taskId,
          edgeCount: 0,
          newEdges: 0,
          brokenEdges: 0,
          cycles: 0,
          durationMs: 0,
        };
        await eventBus?.emit({
          kind: "state.graph",
          schemaVersion: "v3",
          sequence: 0,
          streamId: taskId,
          taskId,
          value: {
            status: "skipped",
            edgeCount: 0,
            newEdges: 0,
            brokenEdges: 0,
            cycles: 0,
            durationMs: 0,
          },
          timestamp: new Date().toISOString(),
        });
        return ok(report);
      }

      for (const f of targets) {
        const src = await readFile(resolve(cwd, f), "utf-8");
        for (const line of src.split("\n")) {
          // Static imports: import ... from "..."
          const m = line.match(/import\s+(?:type\s+)?(?:[\w{},*\s]+)\s+from\s+['"]([^'"]+)['"]/);
          if (m) {
            const kind = line.includes("import type")
              ? ("type-only" as const)
              : ("import" as const);
            const resolved = resolveImport(m[1]!, f, cwd);
            const key = `${f}|${resolved}`;
            if (!seen.has(key)) {
              seen.add(key);
              edges.push({ from: f, to: resolved, kind });
            }
          }
          // require() calls
          const req = line.match(/require\s*\(\s*['"]([^'"]+)['"]\s*\)/);
          if (req) {
            const resolved = resolveImport(req[1]!, f, cwd);
            const key = `${f}|${resolved}`;
            if (!seen.has(key)) {
              seen.add(key);
              edges.push({ from: f, to: resolved, kind: "import" });
            }
          }
          // export ... from "..."
          const exp = line.match(/export\s+(?:[\w{},*\s]+)?\s*from\s+['"]([^'"]+)['"]/);
          if (exp) {
            const resolved = resolveImport(exp[1]!, f, cwd);
            const key = `${f}|${resolved}`;
            if (!seen.has(key)) {
              seen.add(key);
              edges.push({ from: f, to: resolved, kind: "import" });
            }
          }
          // dynamic import(): import("...") - marked separately
          const dyn = line.match(/import\s*\(\s*['"]([^'"]+)['"]\s*\)/);
          if (dyn) {
            const resolved = resolveImport(dyn[1]!, f, cwd);
            const key = `${f}|${resolved}`;
            if (!seen.has(key)) {
              seen.add(key);
              edges.push({ from: f, to: resolved, kind: "dynamic" });
            }
          }
        }
      }

      const brokenEdges = edges.filter((e) => {
        if (e.to.startsWith("node_modules/")) return false;
        const full = resolve(cwd, e.to);
        const rel = relative(cwd, full);
        if (rel.startsWith("..") || rel === "") return true;
        if (existsSync(full)) return false;
        if (tryResolveExtension(full)) return false;
        return true;
      }).length;

      const cycles = detectCycles(edges);

      const report: GraphifyReport = {
        taskId,
        edgeCount: edges.length,
        newEdges: edges.length,
        brokenEdges,
        cycles,
        durationMs: Date.now() - start,
      };
      const status = brokenEdges > 0 ? "fail" : "pass";

      await eventBus?.emit({
        kind: "state.graph",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          status,
          edgeCount: edges.length,
          newEdges: edges.length,
          brokenEdges,
          cycles,
          durationMs: report.durationMs,
        },
        timestamp: new Date().toISOString(),
      });

      return ok(report);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      const report: GraphifyReport = {
        taskId,
        edgeCount: 0,
        newEdges: 0,
        brokenEdges: 1,
        cycles: 0,
        durationMs: Date.now() - start,
      };
      await eventBus?.emit({
        kind: "state.graph",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          status: "error",
          error: message,
          edgeCount: 0,
          newEdges: 0,
          brokenEdges: 1,
          cycles: 0,
          durationMs: report.durationMs,
        },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    }
  }
}

function resolveImport(spec: string, from: string, cwd: string): string {
  if (spec.startsWith(".")) {
    let resolved = relative(cwd, resolve(dirname(resolve(cwd, from)), spec));
    if (!resolved.match(/\.\w+$/)) {
      const fullBase = resolve(cwd, resolved);
      const withExt = tryResolveExtension(fullBase);
      if (withExt) resolved = relative(cwd, withExt);
      else resolved += ".ts";
    }
    return resolved;
  }
  return `node_modules/${spec}`;
}

function detectCycles(edges: EdgeInfo[]): number {
  const graph = new Map<string, string[]>();
  for (const e of edges) {
    const list = graph.get(e.from) ?? [];
    list.push(e.to);
    graph.set(e.from, list);
    if (!graph.has(e.to)) graph.set(e.to, []);
  }

  const WHITE = 0,
    GRAY = 1,
    BLACK = 2;
  const color = new Map<string, number>();
  let cycles = 0;

  function dfs(node: string): void {
    color.set(node, GRAY);
    for (const neighbor of graph.get(node) ?? []) {
      const c = color.get(neighbor) ?? WHITE;
      if (c === GRAY) {
        cycles++;
        continue;
      }
      if (c === WHITE) dfs(neighbor);
    }
    color.set(node, BLACK);
  }

  for (const node of graph.keys()) {
    if ((color.get(node) ?? WHITE) === WHITE) dfs(node);
  }

  return cycles;
}
