import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname, extname } from "node:path";
import type { EventBus } from "@kirkforge/core-events";

// ponytail: regex import-edge extraction, not tree-sitter. Reliably parses the
// static import forms (TS/JS import/re-export/require/dynamic-import, Python
// from/import) that cycle + broken-edge detection needs. It misses dynamic and
// re-export edge cases; tree-sitter is the upgrade path if those matter — adding
// native grammar deps for marginal recall is not justified by the gate today.

interface ImportSpec {
  path: string;
  symbols: string[]; // named + "default"; empty = side-effect/namespace/require
}

type Target =
  | { kind: "internal"; to: string }
  | { kind: "missing" }
  | { kind: "external" };

const JS_EXTS = ["", ".ts", ".tsx", ".mjs", ".cjs", ".js", ".jsx", ".mts", ".cts"];
const INDEX_EXTS = ["/index.ts", "/index.tsx", "/index.mjs", "/index.cjs", "/index.js", "/index.jsx"];
const JS_EXT_SET = new Set([".ts", ".tsx", ".mjs", ".cjs", ".js", ".jsx", ".mts", ".cts"]);

function safeRead(file: string): string | null {
  try {
    return readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

function isRelative(spec: string): boolean {
  return spec.startsWith("./") || spec.startsWith("../") || spec.startsWith("/");
}

function stripComments(src: string): string {
  // ponytail: line/block comment strip so export/import matches inside comments
  // don't create false edges. String contents are left (a quoted "export" is rare
  // and the cost of a full lexer isn't justified for the gate).
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/[^\n]*/g, "")
    .replace(/^\s*#[^\n]*/gm, "");
}

// ponytail: `importName` drops the ` as alias` tail of an import binding. A tiny
// helper beats scattering `[0] ?? ""` guards across the match loops (the package
// is compiled with `noUncheckedIndexedAccess`, so array/index access is
// `T | undefined`).
function importName(part: string): string {
  return (part.split(/\s+as\s+/)[0] ?? "").trim();
}

function parseImportClause(clause: string): string[] {
  const symbols: string[] = [];
  const named = clause.match(/\{([^}]*)\}/);
  if (named) {
    for (const part of (named[1] ?? "").split(",")) {
      const id = importName(part);
      if (id) symbols.push(id);
    }
  }
  // namespace import (`* as ns`): the whole module object; no individual symbol
  // to verify. broken only if the target file is missing, not by symbol name.
  const defaultName = clause.match(/^([A-Za-z_$][\w$]*)/)?.[1];
  if (defaultName && !named?.[0]?.startsWith(defaultName + ",")) {
    // A bare leading identifier before `{` or alone is the default import.
    if (!clause.includes("{") || clause.indexOf(defaultName) < clause.indexOf("{")) {
      symbols.push("default");
    }
  }
  return symbols;
}

function extractImportSpecs(file: string, src: string): ImportSpec[] {
  const ext = extname(file);
  const clean = stripComments(src);
  const specs: ImportSpec[] = [];

  if (JS_EXT_SET.has(ext)) {
    // import [clause] from 'X'
    for (const m of clean.matchAll(/import\s+(?:type\s+)?((?:[^;]*?))\s+from\s+['"]([^'"]+)['"]/g)) {
      specs.push({ path: m[2] ?? "", symbols: parseImportClause(m[1] ?? "") });
    }
    // import 'X'  (side-effect)
    for (const m of clean.matchAll(/import\s+['"]([^'"]+)['"]/g)) {
      const p = m[1] ?? "";
      if (p && !specs.some((s) => s.path === p && s.symbols.length === 0)) {
        specs.push({ path: p, symbols: [] });
      }
    }
    // export ... from 'X'  (re-export)
    for (const m of clean.matchAll(/export\s+(?:type\s+)?(?:[^;]*?)\s+from\s+['"]([^'"]+)['"]/g)) {
      if (m[1]) specs.push({ path: m[1], symbols: [] });
    }
    // require('X')
    for (const m of clean.matchAll(/require\s*\(\s*['"]([^'"]+)['"]\s*\)/g)) {
      if (m[1]) specs.push({ path: m[1], symbols: [] });
    }
    // import('X')  (dynamic)
    for (const m of clean.matchAll(/import\s*\(\s*['"]([^'"]+)['"]\s*\)/g)) {
      if (m[1]) specs.push({ path: m[1], symbols: [] });
    }
  } else if (ext === ".py") {
    // from .x import a, b as c
    for (const m of clean.matchAll(/from\s+(\.+[A-Za-z_][\w.]*)\s+import\s+([^\n]+)/g)) {
      const symbols = (m[2] ?? "")
        .split(",")
        .map(importName)
        .filter(Boolean);
      const p = m[1] ?? "";
      if (p) specs.push({ path: p, symbols });
    }
    // from . import a, b
    for (const m of clean.matchAll(/from\s+(\.+)\s+import\s+([^\n]+)/g)) {
      const p = m[1] ?? "";
      if (p.length > 0 && !specs.some((s) => s.path === p)) {
        const symbols = (m[2] ?? "")
          .split(",")
          .map(importName)
          .filter(Boolean);
        specs.push({ path: p, symbols });
      }
    }
  }
  return specs;
}

function resolveJsTarget(file: string, spec: string): string | null {
  const base = resolve(dirname(file), spec);
  for (const e of JS_EXTS) {
    if (existsSync(base + e)) return base + e;
  }
  for (const e of INDEX_EXTS) {
    if (existsSync(base + e)) return base + e;
  }
  return null;
}

function resolvePyTarget(file: string, spec: string): string | null {
  // spec like ".foo" or "..pkg.foo": leading dots = relative depth.
  const dots = spec.match(/^\.+/)?.[0].length ?? 0;
  let dir = dirname(file);
  for (let i = 1; i < dots; i++) dir = dirname(dir);
  const modPath = spec.slice(dots).replace(/\./g, "/");
  if (!modPath) {
    // `from . import x` -> the package __init__.py
    const init = join(dir, "__init__.py");
    return existsSync(init) ? init : null;
  }
  const dirBase = join(dir, modPath);
  if (existsSync(dirBase + ".py")) return dirBase + ".py";
  if (existsSync(join(dirBase, "__init__.py"))) return join(dirBase, "__init__.py");
  return null;
}

function resolveImportTarget(file: string, spec: string): Target {
  if (!isRelative(spec)) return { kind: "external" };
  const ext = extname(file);
  const to = JS_EXT_SET.has(ext) || ext === "" ? resolveJsTarget(file, spec) : resolvePyTarget(file, spec);
  return to ? { kind: "internal", to } : { kind: "missing" };
}

function symbolExported(targetFile: string, symbol: string): boolean {
  if (!symbol) return true;
  const src = safeRead(targetFile);
  if (src === null) return false;
  const clean = stripComments(src);
  const ext = extname(targetFile);
  if (JS_EXT_SET.has(ext)) {
    if (symbol === "default") return /export\s+default\b/.test(clean);
    const reNamed = new RegExp(`export\\s+\\{[^}]*\\b${escapeRe(symbol)}\\b[^}]*\\}`);
    const reDecl = new RegExp(`export\\s+(?:const|let|var|function|class|interface|type|enum)\\s+${escapeRe(symbol)}\\b`);
    return reNamed.test(clean) || reDecl.test(clean);
  }
  if (ext === ".py") {
    const re = new RegExp(
      `(^|\\n)\\s*(?:def|class)\\s+${escapeRe(symbol)}\\b|(^|\\n)\\s*${escapeRe(symbol)}\\s*=|from\\s+[^\\n]+\\s+import\\s+[^\\n]*\\b${escapeRe(symbol)}\\b`,
    );
    return re.test(clean);
  }
  return true;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// join helper (avoid importing node:path twice)
function join(...parts: string[]): string {
  return parts.join("/").replace(/\/+/g, "/");
}

// DFS back-edge cycle count over the internal-edge graph.
function countCycles(edges: Array<{ from: string; to: string }>): number {
  const adj = new Map<string, string[]>();
  for (const e of edges) {
    const list = adj.get(e.from) ?? [];
    list.push(e.to);
    adj.set(e.from, list);
    if (!adj.has(e.to)) adj.set(e.to, adj.get(e.to) ?? []);
  }
  let cycles = 0;
  const state = new Map<string, 0 | 1 | 2>(); // 0=unvisited,1=in-stack,2=done
  const dfs = (node: string): void => {
    state.set(node, 1);
    for (const next of adj.get(node) ?? []) {
      const s = state.get(next) ?? 0;
      if (s === 1) cycles++;
      else if (s === 0) dfs(next);
    }
    state.set(node, 2);
  };
  for (const node of adj.keys()) {
    if ((state.get(node) ?? 0) === 0) dfs(node);
  }
  return cycles;
}

export class GraphEmitter {
  constructor(
    private opts: { eventBus: EventBus; files?: string[]; writtenFiles?: string[] },
  ) {}

  async emit(taskId: string): Promise<void> {
    const start = Date.now();
    const files = (this.opts.files ?? []).filter((f) => existsSync(f));
    const written = new Set(this.opts.writtenFiles ?? []);

    type Edge = { from: string; to: string | null; missing: boolean; symbols: string[] };
    const edges: Edge[] = [];
    for (const file of files) {
      const src = safeRead(file);
      if (src === null) continue;
      for (const spec of extractImportSpecs(file, src)) {
        const target = resolveImportTarget(file, spec.path);
        if (target.kind === "external") continue;
        edges.push({
          from: file,
          to: target.kind === "internal" ? target.to : null,
          missing: target.kind === "missing",
          symbols: spec.symbols,
        });
      }
    }

    const internalEdges = edges.filter((e) => !e.missing && e.to !== null) as Array<
      Required<Edge> & { to: string }
    >;
    const edgeCount = edges.length;
    const brokenEdges = edges.filter(
      (e) => e.missing || (e.to !== null && e.symbols.some((s) => !symbolExported(e.to as string, s))),
    ).length;
    const newEdges = edges.filter(
      (e) => written.has(e.from) || (e.to !== null && written.has(e.to)),
    ).length;
    const cycles = countCycles(internalEdges.map((e) => ({ from: e.from, to: e.to })));

    await this.opts.eventBus.emit({
      kind: "state.graph",
      schemaVersion: "v3",
      sequence: 0,
      streamId: taskId,
      taskId,
      value: {
        status:
          files.length === 0 ? "skipped" : brokenEdges > 0 || cycles > 0 ? "fail" : "pass",
        edgeCount,
        newEdges,
        brokenEdges,
        cycles,
        durationMs: Date.now() - start,
      },
      timestamp: new Date().toISOString(),
    });
  }
}