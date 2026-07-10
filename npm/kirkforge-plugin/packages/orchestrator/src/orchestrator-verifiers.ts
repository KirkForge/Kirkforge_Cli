import { execFileAsync } from "./orchestrator-validators.js";
import { createVerificationEmitters } from "./emitter-factory.js";
import { detectTaskProfile, profileForLanguage } from "./task-profile.js";
import type { TaskBrief } from "@kirkforge/prompt-core";
import type { TaskInput } from "./types.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/** Run the deterministic verifier emitters (lint/types/security/changes/graph/imports). */
export async function runVerifiers(
  s: OrchestratorInternals,
  taskId: string,
  files?: string[],
  language?: ReturnType<typeof detectTaskProfile>["language"],
  writtenFiles?: string[],
): Promise<void> {
  const profile = language ? profileForLanguage(language) : undefined;
  if (profile?.checkCommand && writtenFiles && writtenFiles.length > 0) {
    await runCheckCommand(s, profile.checkCommand, writtenFiles, taskId, profile.structuredCheck);
  }
  const eb = s.sharedEventBus;
  const emitters = createVerificationEmitters(
    s.cwd,
    eb,
    files,
    language,
    writtenFiles,
  );
  await Promise.allSettled([
    emitters.lint.emit(taskId),
    emitters.types.emit(taskId),
    emitters.security.emit(taskId),
    emitters.changes.emit(taskId),
    emitters.graph.emit(taskId),
    emitters.imports.emit(taskId),
  ]);
}

/**
 * Spawn a check command after delegation. Supports either a structured
 * (command+args) form or a raw shell form (string).
 */
export async function runCheckCommand(
  s: OrchestratorInternals,
  checkCommand: string,
  files: string[],
  taskId: string,
  structured?: { command: string; args: string[]; appendFiles?: boolean },
): Promise<void> {
  if (structured && structured.command) {
    const args =
      structured.appendFiles !== false ? [...structured.args, ...files] : structured.args;
    try {
      const { stdout, stderr } = await execFileAsync(structured.command, args, {
        cwd: s.cwd,
        timeout: 30000,
        maxBuffer: 10 * 1024 * 1024,
      });
      const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
      if (output) {
        s.logger?.info(
          `[orchestrator] checkCommand ${structured.command} passed for ${files.length} file(s)`,
        );
      }
    } catch (e) {
      const errObj = e as { stdout?: string; stderr?: string; message?: string };
      const output = `${errObj.stdout ?? ""}${errObj.stderr ? `\n${errObj.stderr}` : ""}`.trim();
      s.logger?.warn(
        `[orchestrator] checkCommand ${structured.command} failed: ${output || errObj.message || "unknown error"}`,
      );
      await s.sharedEventBus.emit({
        kind: "verify.types",
        schemaVersion: "v3",
        sequence: 1,
        streamId: taskId,
        taskId,
        value: {
          status: "fail",
          errors: 1,
          durationMs: 0,
          details: [
            {
              file: "<checkCommand>",
              line: 0,
              code: "CHECK_CMD_FAIL",
              message: `${structured.command}: ${output || errObj.message || "failed"}`,
            },
          ],
        },
        timestamp: new Date().toISOString(),
      });
    }
    return;
  }
  if (!checkCommand) return;
  const parts = checkCommand.split(/\s+/);
  const cmd = parts[0];
  if (!cmd || files.length === 0) return;
  const args = [...parts.slice(1), ...files];
  try {
    const { stdout, stderr } = await execFileAsync(cmd, args, {
      cwd: s.cwd,
      timeout: 30000,
      maxBuffer: 10 * 1024 * 1024,
    });
    const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
    if (output) {
      s.logger?.info(
        `[orchestrator] checkCommand ${checkCommand} passed for ${files.length} file(s)`,
      );
    }
  } catch (e) {
    const errObj = e as { stdout?: string; stderr?: string; message?: string };
    const output = `${errObj.stdout ?? ""}${errObj.stderr ? `\n${errObj.stderr}` : ""}`.trim();
    s.logger?.warn(
      `[orchestrator] checkCommand ${checkCommand} failed: ${output || errObj.message || "unknown error"}`,
    );
    await s.sharedEventBus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 1,
      streamId: taskId,
      taskId,
      value: {
        status: "fail",
        errors: 1,
        durationMs: 0,
        details: [
          {
            file: "<checkCommand>",
            line: 0,
            code: "CHECK_CMD_FAIL",
            message: `${checkCommand}: ${output || errObj.message || "failed"}`,
          },
        ],
      },
      timestamp: new Date().toISOString(),
    });
  }
}

/** Build the prompt-core TaskBrief for a given task. */
export function makeBrief(s: OrchestratorInternals, task: TaskInput): TaskBrief {
  void s;
  const profile = detectTaskProfile(task.description);
  const forbiddenList =
    profile.forbiddenExtensions.length > 0
      ? `\nForbidden file types: ${profile.forbiddenExtensions.join(", ")}.`
      : "";
  const emissionRules =
    profile.allowedExtensions.length > 0
      ? `\nAllowed file extensions for ${profile.language}: ${profile.allowedExtensions.join(", ")}.`
      : "";
  const contextSection = task.context ? `\nContext: ${task.context}` : "";
  const filesSection =
    task.files && task.files.length > 0 ? `\nTarget files: ${task.files.join(", ")}` : "";
  return {
    description: task.description + contextSection + filesSection,
    variables: {
      files: task.files?.join(", ") ?? "",
      language: profile.language,
      defaultFile: profile.defaultFile,
      languageHint: profile.promptHint,
      checkCommand: profile.checkCommand,
      emissionRules,
      forbiddenRules: forbiddenList,
      context: task.context ?? "",
    },
  };
}
