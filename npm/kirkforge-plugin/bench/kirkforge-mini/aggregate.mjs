// KirkForge Mini-Bench — aggregator.
//
// Reads every cell.json under <run-dir> and emits a markdown report grouped
// by worker (rows) × task (columns). Cells with no JSON are treated as
// "did not run."
//
// Usage:
//   node bench/kirkforge-mini/aggregate.mjs /tmp/kirkforge-mini/run-001
//   node bench/kirkforge-mini/aggregate.mjs /tmp/kirkforge-mini/run-001 > REPORT.md

import { readFile, readdir, writeFile, stat } from "node:fs/promises";
import { join } from "node:path";

const runDir = process.argv[2];
if (!runDir) {
  console.error("Usage: aggregate.mjs <run-dir>");
  process.exit(2);
}

const tasks = [];
const workerCells = new Map(); // workerId → Map(taskName → cell)
const workerTotal = new Map(); // workerId → { passed, failed, totalTokens, totalMs }

// Discover tasks/workers by directory shape: run-dir/run-id/<task>/<worker>/cell.json
const runIdDirs = (await readdir(runDir, { withFileTypes: true })).filter((d) => d.isDirectory());
const latestRunId = runIdDirs.sort((a, b) => b.name.localeCompare(a.name))[0]?.name;
if (!latestRunId) {
  console.error(`No run-id subdirs in ${runDir}`);
  process.exit(2);
}

const runPath = join(runDir, latestRunId);
const taskDirs = (await readdir(runPath, { withFileTypes: true })).filter((d) => d.isDirectory());

for (const t of taskDirs) {
  tasks.push(t.name);
  const workerDirs = (await readdir(join(runPath, t.name), { withFileTypes: true })).filter(
    (d) => d.isDirectory(),
  );
  for (const w of workerDirs) {
    const cellPath = join(runPath, t.name, w.name, "cell.json");
    try {
      const cell = JSON.parse(await readFile(cellPath, "utf8"));
      if (!workerCells.has(w.name)) {
        workerCells.set(w.name, new Map());
        workerTotal.set(w.name, { passed: 0, failed: 0, totalTokens: 0, totalMs: 0 });
      }
      workerCells.get(w.name).set(t.name, cell);
      const tot = workerTotal.get(w.name);
      if (cell.overall.passed) tot.passed++;
      else tot.failed++;
      if (cell.overall.sessionTokens) tot.totalTokens += cell.overall.sessionTokens;
      tot.totalMs += cell.run.wallMs;
    } catch (e) {
      console.error(`WARN: failed to read ${cellPath}: ${e.message}`);
    }
  }
}

tasks.sort();
const workerIds = [...workerCells.keys()].sort();

// ── Render ──────────────────────────────────────────────────────────────
const lines = [];
lines.push(`# KirkForge Mini-Bench Results`);
lines.push("");
lines.push(`- **Run ID**: \`${latestRunId}\``);
lines.push(`- **Source**: \`${runDir}\``);
lines.push(`- **Generated**: ${new Date().toISOString()}`);
lines.push(`- **Tasks**: ${tasks.length} (${tasks.join(", ")})`);
lines.push(`- **Workers**: ${workerIds.length} (${workerIds.join(", ")})`);
lines.push(`- **Total cells**: ${workerIds.length * tasks.length}`);
lines.push("");

// Pass/fail matrix
lines.push(`## Pass / Fail`);
lines.push("");
lines.push(`| Worker | ${tasks.map((t) => t.replace(/^\d+-/, "")).join(" | ")} | Total |`);
lines.push(`| --- | ${tasks.map(() => "---").join(" | ")} | --- |`);
for (const w of workerIds) {
  const cells = workerCells.get(w);
  const cells_for_row = tasks.map((t) => {
    const c = cells.get(t);
    if (!c) return "—";
    return c.overall.passed ? "✓" : "✗";
  });
  const tot = workerTotal.get(w);
  lines.push(`| \`${w}\` | ${cells_for_row.join(" | ")} | ${tot.passed}/${tasks.length} |`);
}
lines.push("");

// Tokens per cell
lines.push(`## Session Tokens`);
lines.push("");
lines.push(`| Worker | ${tasks.map((t) => t.replace(/^\d+-/, "")).join(" | ")} | Total |`);
lines.push(`| --- | ${tasks.map(() => "---").join(" | ")} | --- |`);
for (const w of workerIds) {
  const cells = workerCells.get(w);
  const cells_for_row = tasks.map((t) => {
    const c = cells.get(t);
    if (!c) return "—";
    return String(c.overall.sessionTokens ?? "?");
  });
  const tot = workerTotal.get(w);
  lines.push(`| \`${w}\` | ${cells_for_row.join(" | ")} | ${tot.totalTokens} |`);
}
lines.push("");

// Turns per cell
lines.push(`## Correction Turns`);
lines.push("");
lines.push(`| Worker | ${tasks.map((t) => t.replace(/^\d+-/, "")).join(" | ")} |`);
lines.push(`| --- | ${tasks.map(() => "---").join(" | ")} |`);
for (const w of workerIds) {
  const cells = workerCells.get(w);
  const cells_for_row = tasks.map((t) => {
    const c = cells.get(t);
    if (!c) return "—";
    return String(c.overall.turnCount);
  });
  lines.push(`| \`${w}\` | ${cells_for_row.join(" | ")} |`);
}
lines.push("");

// Wall-clock
lines.push(`## Wall-Clock per Cell (ms)`);
lines.push("");
lines.push(`| Worker | ${tasks.map((t) => t.replace(/^\d+-/, "")).join(" | ")} |`);
lines.push(`| --- | ${tasks.map(() => "---").join(" | ")} |`);
for (const w of workerIds) {
  const cells = workerCells.get(w);
  const cells_for_row = tasks.map((t) => {
    const c = cells.get(t);
    if (!c) return "—";
    return String(c.run.wallMs);
  });
  lines.push(`| \`${w}\` | ${cells_for_row.join(" | ")} |`);
}
lines.push("");

// Per-cell detail
lines.push(`## Per-Cell Detail`);
lines.push("");
for (const t of tasks) {
  lines.push(`### ${t}`);
  lines.push("");
  for (const w of workerIds) {
    const c = workerCells.get(w).get(t);
    if (!c) continue;
    const tag = c.overall.passed ? "PASS" : "FAIL";
    lines.push(
      `- **${w}** → ${tag} | validator_exit=${c.validator.exitCode} | run_exit=${c.run.exitCode} | ` +
        `turns=${c.overall.turnCount} | tokens=${c.overall.sessionTokens ?? "?"} | ` +
        `wall=${c.run.wallMs}ms | verdict=${c.overall.finalVerdict ?? "?"}`,
    );
    if (c.validator.stderrTail && c.validator.exitCode !== 0) {
      lines.push(`  - validator stderr (last 200): \`${c.validator.stderrTail.slice(-200).replace(/\n/g, " ")}\``);
    }
  }
  lines.push("");
}

lines.push(`## Headline numbers`);
lines.push("");
for (const w of workerIds) {
  const tot = workerTotal.get(w);
  const passRate = (tot.passed / tasks.length) * 100;
  const tokensPerPass = tot.passed > 0 ? Math.round(tot.totalTokens / tot.passed) : "—";
  lines.push(
    `- **${w}**: pass rate ${passRate.toFixed(0)}% (${tot.passed}/${tasks.length}), ` +
      `total tokens ${tot.totalTokens}, tokens/pass ${tokensPerPass}, ` +
      `total wall ${Math.round(tot.totalMs / 1000)}s`,
  );
}
lines.push("");

const md = lines.join("\n");
console.log(md);

// Also write next to the run directory for convenience
const outFile = join(runPath, "REPORT.md");
await writeFile(outFile, md);
console.error(`[aggregate] wrote ${outFile}`);
