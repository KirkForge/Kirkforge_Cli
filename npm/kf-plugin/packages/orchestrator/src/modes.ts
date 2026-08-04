import type { Agent } from "@kirkforge/agent-core";
import type { TaskBrief } from "@kirkforge/prompt-core";
import type { DelegationResult } from "./types.js";
import type { OrchestratorResult } from "./types.js";
import { ok, err } from "@kirkforge/core-types";
import {
  renameSync,
  writeFileSync as writeFileSyncRaw,
  mkdirSync,
  readFileSync,
  existsSync,
} from "node:fs";
import { randomBytes } from "node:crypto";
import { resolve, relative, dirname } from "node:path";
import { extensionForLanguage, type TaskProfile } from "./task-profile.js";
import {
  disallowedArtifact,
  segmentsHaveEscapingSymlink,
  finalFileIsSymlink,
} from "./artifact-mode.js";
import type { ParsedArtifact } from "./artifact-mode.js";
import { sha256Of, isBinaryLikeContent, isInsideCwd, MAX_ARTIFACT_BYTES } from "./path-safety.js";

// ── Hard-prompt code-block persistence ─────────────────────────────────────

/**
 * Extracts fenced code blocks from raw model output and persists them
 * into `cwd` using the same safety primitives as artifact-mode.
 */
function persistCodeBlocks(
  content: string,
  cwd: string,
  profile?: TaskProfile,
  targetFile?: string,
  forceOverwrite = false,
): {
  written: string[];
  blocked: Array<{ path: string; reason: string }>;
  hashes: string[];
  fileBytes: number[];
  beforeHashes: (string | null)[];
  existed: boolean[];
} {
  const written: string[] = [];
  const blocked: Array<{ path: string; reason: string }> = [];
  const beforeHashSnapshots = new Map<string, { beforeHash: string | null; existed: boolean }>();
  const blocks = [...content.matchAll(/```([A-Za-z0-9_+#.-]*)\s*\n([\s\S]*?)```/g)];
  if (blocks.length === 0) {
    blocks.push(...content.matchAll(/```\s*\n([\s\S]*?)```/g));
  }
  // When the caller passed a target file (e.g. the harness --file
  // argument), use that as the base name. Otherwise fall back to the
  // profile's defaultFile. This lets the model write to a path the
  // downstream validator actually checks, rather than `output.ts` or
  // `answer-1.txt` chosen by language.
  const ext = extensionForLanguage(profile?.language);
  const baseName = targetFile ?? profile?.defaultFile ?? `output${ext}`;

  // When the caller pinned a target file, models sometimes emit more
  // than one fenced code block (explanation + actual code). Pick the
  // largest non-empty block as "the code" and discard the rest, so
  // the write lands at the expected pinned path rather than at
  // -1/-2/-3 suffixed copies the validator doesn't look for.
  let blockIndices: number[];
  if (targetFile && blocks.length > 1) {
    const sized = blocks
      .map((m, i) => ({ i, size: (m[2] ?? m[1] ?? "").trim().length }))
      .filter((x) => x.size > 0)
      .sort((a, b) => b.size - a.size);
    blockIndices = sized.length > 0 ? [sized[0]!.i] : [];
  } else {
    blockIndices = blocks.map((_, i) => i);
  }

  for (const i of blockIndices) {
    const match = blocks[i]!;
    const code = (match[2] ?? match[1] ?? "").trim();
    let name: string;
    if (targetFile) {
      // Caller pinned a target file (path relative to the workspace,
      // e.g. `src/clamp.ts`). Use it directly so the write lands at
      // the right place inside the isolated turn-workspace.
      name = targetFile;
    } else {
      name = blocks.length === 1 ? baseName : baseName.replace(/\.(\w+)$/, `-${i + 1}.$1`);
    }
    const fp = resolve(cwd, name);

    if (!isInsideCwd(fp, cwd)) {
      blocked.push({ path: name, reason: `path escapes sandbox: ${name}` });
      continue;
    }

    const artifact: ParsedArtifact = { filePath: name, content: code + "\n" };
    const rejection = disallowedArtifact(artifact, profile);
    if (rejection) {
      blocked.push({ path: name, reason: rejection });
      continue;
    }

    if (segmentsHaveEscapingSymlink(fp, cwd)) {
      blocked.push({ path: name, reason: `symlink escape detected: ${name}` });
      continue;
    }

    if (finalFileIsSymlink(fp)) {
      blocked.push({
        path: name,
        reason: `final path is symlink — writes would follow link outside sandbox: ${name}`,
      });
      continue;
    }

    if (Buffer.byteLength(code + "\n", "utf-8") > MAX_ARTIFACT_BYTES) {
      blocked.push({
        path: name,
        reason: `artifact exceeds ${MAX_ARTIFACT_BYTES} byte limit: ${name}`,
      });
      continue;
    }

    if (isBinaryLikeContent(code)) {
      blocked.push({ path: name, reason: `binary-like content detected: ${name}` });
      continue;
    }

    // Enforce write policy — overwrite requires explicit opt-in
    const existed = existsSync(fp);
    if (existed && !forceOverwrite && profile?.writePolicy?.allowOverwrite !== true) {
      blocked.push({
        path: name,
        reason: `overwrite denied (allowOverwrite not enabled in writePolicy): ${name}`,
      });
      continue;
    }
    if (profile?.writePolicy?.denyPaths) {
      const relPath = relative(cwd, fp);
      if (profile.writePolicy.denyPaths.some((d) => relPath === d || relPath.startsWith(d + "/"))) {
        blocked.push({ path: name, reason: `path denied by writePolicy: ${name}` });
        continue;
      }
    }

    try {
      mkdirSync(dirname(fp), { recursive: true });
      // Snapshot beforeHash BEFORE the atomic write
      beforeHashSnapshots.set(
        name,
        (() => {
          try {
            const prevContent = readFileSync(fp, "utf-8");
            return { beforeHash: sha256Of(prevContent), existed: true };
          } catch {
            return { beforeHash: null, existed: false };
          }
        })(),
      );
      const tmpPath = fp + ".tmp." + Date.now() + "." + randomBytes(4).toString("hex");
      writeFileSyncRaw(tmpPath, code + "\n", "utf-8");
      renameSync(tmpPath, fp);
      written.push(name);
    } catch {
      blocked.push({ path: name, reason: `write error: ${name}` });
    }
  }
  const hashes: string[] = [];
  const fileBytes: number[] = [];
  const beforeHashes: (string | null)[] = [];
  const existed: boolean[] = [];
  for (const name of written) {
    const fullPath = resolve(cwd, name);
    const snapshot = beforeHashSnapshots.get(name);
    try {
      const fileContent = readFileSync(fullPath, "utf-8");
      hashes.push(sha256Of(fileContent));
      fileBytes.push(Buffer.byteLength(fileContent, "utf-8"));
    } catch {
      hashes.push("");
      fileBytes.push(0);
    }
    beforeHashes.push(snapshot?.beforeHash ?? null);
    existed.push(snapshot?.existed ?? false);
  }
  return { written, blocked, hashes, fileBytes, beforeHashes, existed };
}

// ── Mode executors ─────────────────────────────────────────────────────────

export async function executeHardPrompt(
  agent: Agent,
  brief: TaskBrief,
  taskId: string,
  cwd: string = process.cwd(),
  profile?: TaskProfile,
  targetFile?: string,
): Promise<OrchestratorResult> {
  const result = await agent.execute(brief);
  if (!result.ok) return err(result.error);
  const emission = result.value;

  const wasTruncated = emission.finishReason === "length" || emission.finishReason === "max_tokens";
  // When the caller passed a target file (e.g. harness --file), the
  // explicit destination implies overwrite is intended (the worker is
  // being asked to fix/replace that file). Without this, the write
  // would be blocked by the writePolicy.allowOverwrite guard for any
  // pre-existing file. See bench/kirkforge-mini RESULTS.md for the
  // symptom and the context.
  const forceOverwrite = targetFile !== undefined;
  const { written, blocked, hashes, fileBytes, beforeHashes, existed } = wasTruncated
    ? { written: [], blocked: [], hashes: [], fileBytes: [], beforeHashes: [], existed: [] }
    : persistCodeBlocks(emission.content, cwd, profile, targetFile, forceOverwrite);
  const truncationWarning = wasTruncated
    ? `model output was truncated (finish_reason: ${emission.finishReason}) — file content may be incomplete`
    : undefined;

  const signals: DelegationResult["signals"] = [
    {
      id: `sig-${taskId}`,
      taskId,
      domain: "task",
      kind: "emission",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { content: emission.content.slice(0, 200) },
    },
    {
      id: `sig-files-${taskId}`,
      taskId,
      domain: "code",
      kind: "files.written",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: {
        files: written.map((w, i) => ({
          path: w,
          sha256: hashes[i] ?? "",
          bytes: fileBytes[i] ?? 0,
          beforeHash: beforeHashes[i] ?? null,
          existed: existed[i] ?? false,
        })),
        language: profile?.language ?? "unknown",
      },
    },
    // The reducer (orchestrator-correction.ts:136) reads `artifact.emitted`
    // signals to build `packet.emissions.files`, which the validator
    // pipeline uses to overlay worker-written files into the isolated
    // validator workspace. Without this signal, hard-prompt mode writes
    // the file to the user's workspace but the validator runs in an
    // empty temp dir and never sees the change. See bench/kirkforge-mini
    // RESULTS.md "Why every cell failed" for the symptom.
    {
      id: `sig-emitted-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.emitted",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: {
        filesWritten: written.length,
        totalBytes: fileBytes.reduce((a, b) => a + b, 0),
        files: written.map((w, i) => ({
          path: w,
          sha256: hashes[i] ?? "",
          bytes: fileBytes[i] ?? 0,
          beforeHash: beforeHashes[i] ?? null,
          existed: existed[i] ?? false,
        })),
        language: profile?.language ?? "unknown",
      },
    },
  ];
  if (blocked.length > 0) {
    signals.push({
      id: `sig-blocked-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.blocked",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { blockedPaths: blocked.map((b) => ({ path: b.path, reason: b.reason })) },
    });
  }
  if (truncationWarning) {
    signals.push({
      id: `sig-truncated-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.truncated",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { finishReason: emission.finishReason, warnings: [truncationWarning] },
    });
  }

  const dr: DelegationResult = {
    decision: {
      mode: "hard-prompt",
      reason: `hard-prompt delegation: ${written.length} files written${blocked.length > 0 ? `, ${blocked.length} blocked` : ""}`,
      autoRouted: true,
    },
    emission,
    signals,
  };
  return ok(dr);
}

export async function executeSchemaContract(
  agent: Agent,
  brief: TaskBrief,
  taskId: string,
): Promise<OrchestratorResult> {
  const result = await agent.execute(brief);
  if (!result.ok) return err(result.error);
  const emission = result.value;
  if (!emission.schemaContract)
    return err(
      new Error("Schema-Contract delegation failed: no valid schema extraction after retry"),
    );
  const wasTruncated = emission.finishReason === "length" || emission.finishReason === "max_tokens";
  const truncationWarning = wasTruncated
    ? `model output was truncated (finish_reason: ${emission.finishReason}) — schema contract output may be incomplete`
    : undefined;
  const dr: DelegationResult = {
    decision: { mode: "schema-contract", reason: "structured verification", autoRouted: true },
    emission,
    signals: [
      {
        id: `sig-${taskId}`,
        taskId,
        domain: "task",
        kind: "emission",
        source: emission.agentId,
        ts: new Date().toISOString(),
        value: { content: emission.content.slice(0, 200) },
      },
      {
        id: `sig-ts-${taskId}`,
        taskId,
        domain: "quality",
        kind: "schema.validated",
        source: emission.agentId,
        ts: new Date().toISOString(),
        value: { validated: true },
        confidence: wasTruncated ? 0.4 : 0.95,
      },
    ],
  };
  if (wasTruncated) {
    dr.signals.push({
      id: `sig-truncated-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.truncated",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { finishReason: emission.finishReason, warnings: [truncationWarning!] },
    });
  }
  return ok(dr);
}
