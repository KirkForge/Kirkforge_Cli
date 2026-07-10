import { createTSLintEngine } from "@kirkforge/tool-lint-ts";
import { createPyLintEngine } from "@kirkforge/tool-lint-py";
import { createShLintEngine } from "@kirkforge/tool-lint-sh";
import { createCLintEngine } from "@kirkforge/tool-lint-c";
import { createRsLintEngine } from "@kirkforge/tool-lint-rs";
import { createGoLintEngine } from "@kirkforge/tool-lint-go";
import { createSqlLintEngine } from "@kirkforge/tool-lint-sql";
import { createImportLintEngine } from "@kirkforge/tool-lint-imports";
import { TscEmitter } from "@kirkforge/tool-tsc";
import { PyrightEmitter } from "@kirkforge/tool-pyright";
import { GitnexusEmitter } from "@kirkforge/tool-gitnexus";
import { GraphifyEmitter } from "@kirkforge/tool-graphify";
import type { EventBus } from "@kirkforge/core-events";
import type { TaskLanguage } from "./task-profile.js";

function hasJsTs(files?: string[]): boolean {
  return (files ?? []).some((file) => /\.(?:[cm]?js|jsx|ts|tsx)$/.test(file));
}

export function createVerificationEmitters(
  cwd: string,
  eventBus: EventBus,
  files?: string[],
  language?: TaskLanguage,
  writtenFiles?: string[],
) {
  const pythonOnly = language === "python" || (!language && !hasJsTs(files));

  // Phase 1+2+3: KirkForge native strict lint for all supported languages
  const tsLint = createTSLintEngine({ cwd, eventBus, files });
  const pyLint = createPyLintEngine({ cwd, eventBus, files });

  // Phase 3: Language-specific native lint
  const lintByLang: Record<string, { emit: (taskId: string) => ReturnType<typeof tsLint.emit> }> = {
    shell: createShLintEngine({ cwd, eventBus, files }),
    c: createCLintEngine({ cwd, eventBus, files }),
    cpp: createCLintEngine({ cwd, eventBus, files }),
    rust: createRsLintEngine({ cwd, eventBus, files }),
    go: createGoLintEngine({ cwd, eventBus, files }),
    sql: createSqlLintEngine({ cwd, eventBus, files }),
    text: tsLint, // fallback to TS for text
  };

  const resolvedLint =
    language && lintByLang[language] ? lintByLang[language]! : pythonOnly ? pyLint : tsLint;
  // Security is now handled by the lint engine itself (emits verify.security for safety-category rules)
  const resolvedSecurity = resolvedLint;

  // Imports verifier: runs on both Python and TypeScript workspaces. Emits
  // verify.imports as an advisory slot by default — the reducer treats it
  // as a warning source, not a fail-closed hard fail.
  const imports = createImportLintEngine({ cwd, eventBus, files });

  return {
    lint: resolvedLint,
    types: pythonOnly
      ? new PyrightEmitter({ cwd, eventBus, files })
      : new TscEmitter({ cwd, eventBus, files }),
    security: resolvedSecurity,
    changes: new GitnexusEmitter({ cwd, eventBus, writtenFiles }),
    graph: new GraphifyEmitter({ cwd, eventBus, files }),
    imports,
  };
}
