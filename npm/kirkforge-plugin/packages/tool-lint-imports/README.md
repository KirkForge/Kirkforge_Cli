# @kirkforge/tool-lint-imports

Curated import-rename verifier. Detects Python and TypeScript / JavaScript imports that point to packages which have been renamed, deprecated, or removed, and emits them as a `verify.imports` event. Wired into the orchestrator as an **advisory** slot — it never fail-closes a build on its own.

## Why

The known blind spot in [ADR-005](../../docs/adr/005-cheap-worker-thesis.md): a worker model that imports `PyPDF2`, `urllib2`, `distutils`, or `request` (Node) generates code that looks plausible to a verifier but will fail at runtime or produce a deprecated-API lint hit. The correction loop is bad at catching this class because no `tsc` / `pyright` / `ruff` / `bandit` check fires on import names — they're syntactically valid.

This package gives the orchestrator a deterministic look-up against a curated table of well-known renames. A clean import passes; a deprecated one is flagged with the recommended replacement.

## What's covered

**Python** (~17 entries): `PyPDF2`→`pypdf`, `sklearn`→`scikit-learn`, `PIL`→`Pillow`, `BeautifulSoup`→`bs4`, `urllib2`→`urllib.request`, `urlparse`→`urllib.parse`, `distutils`→`setuptools`, `imp`→`importlib`, `optparse`→`argparse`, `commands`→`subprocess`, `md5`/`sha`→`hashlib`, `htmllib`→`html.parser`, `mutex`→`threading`, `new`→`types.SimpleNamespace`, `robotparser`→`urllib.robotparser`, `whichdb`→`dbm`.

**TypeScript / JavaScript** (~11 entries): `request`→`undici`/`node-fetch`, `mkdirp`→`fs.mkdir({recursive:true})`, `rimraf`→`fs.rm({recursive:true})`, `nock`→`msw`, `moment`→`date-fns`/`luxon`/`Day.js`, `glob`→`fast-glob`/`tinyglobby`, `tslib`→built-in, `node-uuid`→`uuid`, `babel-polyfill`→`core-js`, `q`→native Promises, `axios-retry`→`axios` interceptors.

Node built-ins (`node:fs`, `node:path`) and relative imports (`./foo`, `../bar`) are always ignored.

## Usage

### As part of the orchestrator (default wiring)

The orchestrator's `createVerificationEmitters()` factory in `packages/orchestrator/src/emitter-factory.ts` instantiates `createImportLintEngine()` automatically. When you call `verifyWorkspace()` from `@kirkforge/plugin`, this emitter runs in parallel with the others and emits a `verify.imports` event. The reducer folds it into `ReducedStatePacket.verification.imports`.

By default, this slot is **advisory** — it bumps `overall: "warn"` but does not fail the build. Operators who want it to fail-closed can opt in by adding `imports` to `VerifierPolicy.required` in the policy bundle.

### Standalone

```ts
import { createImportLintEngine } from "@kirkforge/tool-lint-imports";
import { EventBus } from "@kirkforge/core-events";

const eventBus = new EventBus({ bufferCapacity: 1000 });
const engine = createImportLintEngine({ cwd: "/path/to/repo", eventBus });
const result = await engine.emit("task-1");

if (result.ok) {
  console.log("Status:", result.value.status); // "pass" | "warn"
  console.log("Findings:", result.value.findings);
  for (const d of result.value.details) {
    console.log(`  ${d.file}:${d.line}  ${d.oldName} → ${d.newName}`);
  }
}
```

### Custom rename table

```ts
const custom = {
  "internal-legacy": { replacedBy: "internal-new", deprecatedSince: "2024", reason: "Internal rename" },
};
const engine = createImportLintEngine({
  cwd: "/path",
  pythonRenames: custom, // overrides the bundled table
});
```

## Severity

All findings are `severity: "warning"`. The rationale: an import to a deprecated package is a **review item**, not necessarily a bug — the file may be intentionally pinned for compatibility, or the project may be in the middle of a migration. A warning is the right escalation level; `error` would block builds that are otherwise valid.

## Stable

This package is **Stable**. The data tables are the only surface that may grow as new renames become well-known; the `ImportLintEngine` shape, the `verify.imports` event shape, and the reducer's handling are stable.
