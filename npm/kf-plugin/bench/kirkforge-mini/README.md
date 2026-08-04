# KirkForge Mini-Bench

A reproducible, in-repo benchmark for the **cost thesis** of the
KirkForge correction loop: "a cheap worker + a deterministic verifier +
a few correction turns is cheaper and more reliable than one expensive
single-shot call." The headline number the bench is built to surface is
**tokens-per-passing-task** (lower is better).

## What it is

A self-contained mini-bench of 4 hand-written coding tasks (2
TypeScript, 2 Python) that the worker must fix or augment. Each task
ships with:

- `task.json` — the brief, expected files, validator command
- `validator.sh` — a deterministic shell-based test
- `fixtures/starter-*` — the buggy/incomplete starting state the worker
  is given

The harness copies fixtures into an isolated workspace per (task ×
worker) cell, runs `kirkforge run` with the task brief and the task's
validator, and captures per-cell results.

## What it is NOT

- **Not a replacement for TBench.** TBench is the external Testsuite
  used in the larger `bench/` workflows. This bench is in-repo, hand-
  written, and reproducible without external fixtures. It is the
  baseline; TBench is the stress test.
- **Not a model leaderboard.** The bench is the wrong shape for that
  (only 4 tasks). Use an external leaderboard for cross-model
  comparison.
- **Not an enterprise benchmark.** It uses a single tenant and the dev-
  mode `internal` actor. RBAC, audit, and policy are not in scope here.

## Layout

```
bench/kirkforge-mini/
├── README.md                 ← you are here
├── harness.mjs               ← runs every (task × worker) cell
├── aggregate.mjs             ← turns per-cell JSON into a markdown report
├── workers.json              ← the worker configs (model, baseUrl, provider)
├── tasks/
│   ├── 01-typescript-bug-fix/
│   ├── 02-typescript-add-function/
│   ├── 03-python-data-transform/
│   └── 04-python-fix-import/  ← designed to trip the import-name verifier
└── .gitignore                ← ignores /runs/
```

## How to run

From the repo root:

```bash
# Build so the harness can import from packages/*/dist
npm run build

# 1) Pick a worker (edit workers.json if needed)
# 2) Run the harness
node bench/kirkforge-mini/harness.mjs \
  --workers bench/kirkforge-mini/workers.json \
  --tasks bench/kirkforge-mini/tasks \
  --output /tmp/kirkforge-mini/run-001

# 3) Aggregate
node bench/kirkforge-mini/aggregate.mjs /tmp/kirkforge-mini/run-001
```

The aggregate output is also written to
`/tmp/kirkforge-mini/run-001/<run-id>/REPORT.md`.

## Tasks

| ID                          | Language   | Difficulty | What the worker must do                          |
| --------------------------- | ---------- | ---------- | ------------------------------------------------ |
| `01-typescript-bug-fix`     | TypeScript | easy       | Fix the inverted `>`/`<` in `clamp()`            |
| `02-typescript-add-function`| TypeScript | medium     | Add a `unique()` function without using `Set`    |
| `03-python-data-transform`  | Python     | easy       | Rewrite `normalize_name()` to lowercase + clean  |
| `04-python-fix-import`      | Python     | medium     | Replace `PyPDF2` with `pypdf` and update call site |

The `04` task is deliberately written around the import-name verifier
(Thread 4). The starter code imports `PyPDF2`; a worker that doesn't
recognize the deprecation will get caught by the lint pass and the
correction loop will surface the fix.

## Adding a new task

1. Create `tasks/<NN>-<short-name>/` with:
   - `task.json` — `{ id, description, language, expectedFiles, validatorCommand, passCondition, difficulty, estimatedTokens }`
   - `validator.sh` — exits 0 on pass, non-zero on fail. Files in the
     workspace are checked relative to PWD (the harness sets CWD).
   - `fixtures/starter-<destination-path>` — the buggy/incomplete files.
     The `starter-` prefix is stripped on copy.

2. The harness will pick it up automatically — no registration step.

## Adding a new worker

Append to `workers.json`:

```json
{
  "id": "my-worker",
  "provider": "local-ollama",
  "baseUrl": "http://localhost:11434/v1",
  "model": "my-model:tag",
  "maxTokens": 2048,
  "timeoutMs": 120000
}
```

The `provider` must match a key in the model-config provider map. For
this bench we use only `local-ollama` because it's the only provider
that doesn't need an API key in the dev environment.

## What the headline number looks like

The aggregated report shows:

- **Pass / fail matrix** — one row per worker, one column per task
- **Session tokens** — total tokens used by the correction loop per cell
- **Turns** — how many correction attempts the verifier forced
- **Wall-clock** — end-to-end run time
- **Tokens per pass** — the headline cost-thesis number

A single-worker run (the current configuration) gives one row; the
`tokens/pass` number is the baseline. With multiple workers, the
report makes the comparison obvious.
