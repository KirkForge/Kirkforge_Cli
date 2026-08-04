// KirkForge Mini-Bench — harness.
//
// Runs every (task × worker) cell in an isolated workspace, captures the
// run JSON, runs the task's validator, and writes per-cell results.
//
// Usage:
//   node bench/kirkforge-mini/harness.mjs \
//     --workers bench/kirkforge-mini/workers.json \
//     --tasks bench/kirkforge-mini/tasks \
//     --output /tmp/kirkforge-mini/run-001
//
// What it does per cell:
//   1. mkdir /tmp/kirkforge-mini/<run>/<task>/<worker>/
//   2. Copy starter files from task's fixtures/ to the workspace, renaming
//      them to their canonical paths (e.g. starter-src-clamp.ts → src/clamp.ts)
//   3. Invoke `kirkforge run <description> --file <paths> --validator <path>
//      --json` with model env vars set from the worker config
//   4. Run the task's validator.sh from the workspace
//   5. Write a per-cell JSON with the full outcome

import { readFile, writeFile, mkdir, copyFile, readdir, stat } from "node:fs/promises";
import { join, dirname, basename, relative } from "node:path";
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const REPO_TS_CLI = join(REPO_ROOT, "apps/cli/src/index.ts");

// ── Args ────────────────────────────────────────────────────────────────
// Support both `--key=value` and `--key value` (where the value is the
// next non-flag argument).
const rawArgs = process.argv.slice(2);
const args = {};
for (let i = 0; i < rawArgs.length; i++) {
  const a = rawArgs[i];
  if (!a.startsWith("--")) continue;
  const eq = a.indexOf("=");
  let k, v;
  if (eq >= 0) {
    k = a.slice(2, eq);
    v = a.slice(eq + 1);
  } else {
    k = a.slice(2);
    const next = rawArgs[i + 1];
    if (next !== undefined && !next.startsWith("--")) {
      v = next;
      i++;
    } else {
      v = "true";
    }
  }
  args[k] = v;
}

if (args.help || args.h) {
  console.log("Usage: harness.mjs --workers <path> --tasks <dir> --output <dir>");
  process.exit(0);
}

const WORKERS_PATH = args.workers ?? "bench/kirkforge-mini/workers.json";
const TASKS_DIR = args.tasks ?? "bench/kirkforge-mini/tasks";
const OUTPUT_DIR = args.output ?? `/tmp/kirkforge-mini/run-${Date.now()}`;
const RUN_TIMEOUT_MS = Number(args["run-timeout-ms"] ?? 300_000);

await mkdir(OUTPUT_DIR, { recursive: true });
console.log(`[harness] output → ${OUTPUT_DIR}`);

// ── Load workers + tasks ────────────────────────────────────────────────
const workersDoc = JSON.parse(await readFile(WORKERS_PATH, "utf8"));
const workers = workersDoc.workers ?? workersDoc;
console.log(`[harness] ${workers.length} worker(s): ${workers.map((w) => w.id).join(", ")}`);

const taskEntries = (await readdir(TASKS_DIR, { withFileTypes: true }))
  .filter((d) => d.isDirectory())
  .map((d) => d.name)
  .sort();
console.log(`[harness] ${taskEntries.length} task(s): ${taskEntries.join(", ")}`);

if (workers.length === 0) {
  console.error("FATAL: no workers configured in workers.json");
  process.exit(2);
}
if (taskEntries.length === 0) {
  console.error(`FATAL: no tasks in ${TASKS_DIR}`);
  process.exit(2);
}

// ── Helpers ─────────────────────────────────────────────────────────────
/**
 * Copy a task's starter fixtures into the workspace. Fixture files are
 * named `starter_<path>` where `<path>` uses `_` in place of `/` to encode
 * nested paths. We strip the `starter_` prefix and convert `_` back to
 * `/` when copying. Files NOT starting with `starter_` are ignored
 * (reference implementations live alongside starters but are never copied
 * to the worker's workspace).
 */
async function setupWorkspace(workspaceDir, taskDir) {
  const fixturesDir = join(taskDir, "fixtures");
  if (!existsSync(fixturesDir)) return [];
  const fixtures = await readdir(fixturesDir);
  const canonical = [];
  for (const fx of fixtures) {
    if (!fx.startsWith("starter_")) continue;
    const rel = fx.replace(/^starter_/, "");
    const dest = rel.replace(/_/g, "/");
    const destPath = join(workspaceDir, dest);
    await mkdir(dirname(destPath), { recursive: true });
    await copyFile(join(fixturesDir, fx), destPath);
    canonical.push(dest);
  }
  return canonical;
}

function runChild(cmd, args, opts) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, { ...opts, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (c) => (stdout += c.toString()));
    child.stderr.on("data", (c) => (stderr += c.toString()));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolve({ code: -1, stdout, stderr: stderr + "\n[TIMEOUT after " + opts.timeoutMs + "ms]" });
    }, opts.timeoutMs);
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolve({ code: code ?? 0, stdout, stderr });
    });
    child.on("error", (err) => {
      clearTimeout(timer);
      resolve({ code: -1, stdout, stderr: err.message });
    });
  });
}

// ── Run a single cell ──────────────────────────────────────────────────
async function runCell(runId, taskName, worker) {
  const taskDir = join(TASKS_DIR, taskName);
  const taskDoc = JSON.parse(await readFile(join(taskDir, "task.json"), "utf8"));
  // Absolute path to the validator — works regardless of CWD
  const validatorPath = join(REPO_ROOT, TASKS_DIR, taskName, "validator.sh");

  const cellDir = join(OUTPUT_DIR, runId, taskName, worker.id);
  await mkdir(cellDir, { recursive: true });
  const workspace = join(cellDir, "workspace");
  await mkdir(workspace, { recursive: true });

  // Copy starter fixtures
  const canonicalFiles = await setupWorkspace(workspace, taskDir);

  // Build env: provider config from worker, plus OLLAMA_BASE_URL so the
  // bootstrap picks it up. We strip all other provider keys so they
  // don't sneak in.
  const env = { ...process.env };
  for (const k of [
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_DEFAULT_MODEL",
    "OPENROUTER_API_KEY",
    "OPENROUTER_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "NVIDIA_API_KEY",
    "DEEPSEEK_API_KEY",
  ]) {
    delete env[k];
  }
  env.OLLAMA_BASE_URL = worker.baseUrl;
  env.OLLAMA_DEFAULT_MODEL = worker.model;
  env.OLLAMA_API_KEY = worker.apiKey ?? "ollama";
  env.MODEL_DEFAULT_PROVIDER = worker.provider;
  // Allow --validator-shell. The CLI uses ALLOW_UNSAFE_SHELL_VALIDATOR
  // but the orchestrator's validator runner uses ALLOW_UNSAFE_VALIDATOR_SHELL.
  // We set BOTH so the gate passes at every layer.
  env.ALLOW_UNSAFE_SHELL_VALIDATOR = "true";
  env.ALLOW_UNSAFE_VALIDATOR_SHELL = "1";
  // Point the policy engine at the bench's policy file. Without this the
  // model is denied by default ("No model allowlist configured").
  // Respect an externally-set POLICY_FILE_PATH so the same harness can run
  // against a local-only policy (e.g. policy-2026-07-20.json).
  env.POLICY_FILE_PATH = process.env.POLICY_FILE_PATH ?? join(REPO_ROOT, "bench/kirkforge-mini/policy.json");

  // Build the kirkforge run command. Use --file to point at canonical
  // files, --validator-shell (raw) to invoke the task's bash validator
  // with ALLOW_UNSAFE_SHELL_VALIDATOR=true since the task validators are
  // trusted, sandboxed test scripts.
  const runArgs = [
    "tsx",
    REPO_TS_CLI,
    "run",
    taskDoc.description,
    "--mode",
    "hard-prompt",
    "--max-corrections",
    "2",
    "--validator-shell",
    `bash "${validatorPath}"`,
    "--validator-timeout-ms",
    "60000",
    "--json",
    "--language",
    taskDoc.language,
    "--verifier-policy",
    // Skip lint/types/security verifiers — they need a bootstrapped
    // project (npx tsc requires node_modules) and are noise for the
    // bench. We only care about the task validator here.
    JSON.stringify({ required: [], advisory: [] }),
    "--provider",
    worker.provider,
  ];
  for (const f of canonicalFiles) {
    // Pass the RELATIVE path (relative to the harness workspace) so
    // the orchestrator's isolated turn-workspace write target is just
    // `src/clamp.ts` (not `/tmp/.../workspace/src/clamp.ts`, which
    // would get blocked by the sandbox check). The relative path is
    // also the path the validator checks for.
    runArgs.push("--file", f);
  }

  const cellStart = performance.now();
  const runResult = await runChild("npx", runArgs, {
    cwd: workspace,
    env,
    timeoutMs: RUN_TIMEOUT_MS,
  });
  const runWallMs = performance.now() - cellStart;

  // Parse the run JSON
  let runJson = null;
  let parseError = null;
  if (runResult.stdout.trim()) {
    try {
      runJson = JSON.parse(runResult.stdout);
    } catch (e) {
      parseError = e instanceof Error ? e.message : String(e);
    }
  }

  // Run the validator directly so we always know pass/fail, even if the
  // correction loop didn't get there.
  const validatorStart = performance.now();
  const validatorResult = await runChild("bash", [validatorPath], {
    cwd: workspace,
    env,
    timeoutMs: 90_000,
  });
  const validatorMs = performance.now() - validatorStart;

  const cellReport = {
    task: taskName,
    taskDescription: taskDoc.description,
    worker: { id: worker.id, model: worker.model, provider: worker.provider },
    run: {
      command: `npx ${runArgs.join(" ")}`,
      wallMs: Math.round(runWallMs),
      exitCode: runResult.code,
      stdoutBytes: runResult.stdout.length,
      stderrTail: runResult.stderr.slice(-2000),
      parsedJson: runJson,
      parseError,
    },
    validator: {
      command: `bash ${validatorPath}`,
      wallMs: Math.round(validatorMs),
      exitCode: validatorResult.code,
      // The orchestrator's hard-prompt mode writes worker output to
      // its isolated turn-workspace, NOT to the harness's workspace.
      // The harness's "direct" validator (run in the harness
      // workspace) therefore sees the unchanged starter file. The
      // *orchestrator's* task-validator runs in the isolated
      // workspace with the worker's emitted files overlaid, so it
      // sees the actual fix. That's the authoritative pass/fail.
      // We keep the direct-validator field for diagnostic
      // comparison but use the orchestrator's `taskPass` for the
      // overall verdict.
      passed: validatorResult.code === 0,
      stdoutTail: validatorResult.stdout.slice(-1000),
      stderrTail: validatorResult.stderr.slice(-1000),
    },
    overall: {
      // Prefer the orchestrator's taskPass; fall back to direct
      // validator if the JSON didn't parse.
      passed:
        runJson?.taskPass === true
          ? true
          : runJson?.taskPass === false
            ? false
            : validatorResult.code === 0,
      sessionTokens: runJson?.sessionTokens ?? null,
      sessionCost: runJson?.sessionCost ?? null,
      turnCount: runJson?.turns?.length ?? 0,
      finalVerdict: runJson?.finalVerdict ?? null,
      sourceOfTruth: runJson?.sourceOfTruth ?? null,
      taskValidationReason: runJson?.taskValidation?.reason ?? null,
    },
    workspace,
  };

  await writeFile(join(cellDir, "cell.json"), JSON.stringify(cellReport, null, 2));
  return cellReport;
}

// ── Drive all cells ────────────────────────────────────────────────────
const runId = `run-${new Date().toISOString().replace(/[:.]/g, "-")}`;
const cells = [];
for (const taskName of taskEntries) {
  for (const worker of workers) {
    process.stdout.write(`[cell] ${taskName} × ${worker.id} … `);
    const cell = await runCell(runId, taskName, worker);
    cells.push(cell);
    const tag = cell.overall.passed ? "✓" : "✗";
    process.stdout.write(
      `${tag} validator=${cell.validator.exitCode} run=${cell.run.exitCode} ` +
        `turns=${cell.overall.turnCount} tokens=${cell.overall.sessionTokens ?? "?"} ` +
        `(${cell.run.wallMs}ms)\n`,
    );
  }
}

const summary = {
  runId,
  startedAt: new Date().toISOString(),
  workers: workers.map((w) => w.id),
  tasks: taskEntries,
  totalCells: cells.length,
  passed: cells.filter((c) => c.overall.passed).length,
  failed: cells.filter((c) => !c.overall.passed).length,
};
await writeFile(join(OUTPUT_DIR, runId, "summary.json"), JSON.stringify(summary, null, 2));
console.log(`\n[harness] ${summary.passed}/${summary.totalCells} cells passed`);
console.log(`[harness] summary → ${join(OUTPUT_DIR, runId, "summary.json")}`);
