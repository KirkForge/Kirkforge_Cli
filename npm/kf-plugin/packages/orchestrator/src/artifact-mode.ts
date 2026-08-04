import type { Agent } from "@kirkforge/agent-core";
import type { TaskBrief } from "@kirkforge/prompt-core";
import type { DelegationResult } from "./types.js";
import type { OrchestratorResult } from "./types.js";
import { ok, err } from "@kirkforge/core-types";
import type { TaskProfile } from "./task-profile.js";
import {
  writeArtifacts,
  disallowedArtifact,
  segmentsHaveEscapingSymlink,
  finalFileIsSymlink,
  sha256Of,
} from "./path-safety.js";
import type { WriteResult } from "./path-safety.js";

// ── Marker-based artifact parsing ──────────────────────────────────────────

const FILE_MARKER = /^### FILE:\s*(.+)$/;
const END_MARKER = /^### END\s*$/;

export interface ParsedArtifact {
  filePath: string;
  content: string;
}

export interface ParseResult {
  artifacts: ParsedArtifact[];
  strictTermination: boolean;
  warnings: string[];
}

export function parseArtifacts(output: string): ParseResult {
  const artifacts: ParsedArtifact[] = [];
  const warnings: string[] = [];
  const lines = output.split("\n");
  let currentPath: string | null = null;
  let currentContent: string[] = [];
  for (const line of lines) {
    const fm = line.match(FILE_MARKER);
    if (fm) {
      if (currentPath !== null && currentContent.length > 0) {
        const hadMarkerInContent = currentContent.some((cl) => FILE_MARKER.test(cl));
        if (hadMarkerInContent) {
          warnings.push(
            `artifact "${currentPath}" content contained a line matching "### FILE:" — possible marker collision, file may be truncated`,
          );
        }
        artifacts.push({
          filePath: currentPath,
          content: stripOuterFence(currentContent.join("\n")),
        });
      }
      currentPath = fm[1]!.trim();
      currentContent = [];
      continue;
    }
    if (END_MARKER.test(line)) {
      if (currentPath !== null) {
        artifacts.push({
          filePath: currentPath,
          content: stripOuterFence(currentContent.join("\n")),
        });
        currentPath = null;
        currentContent = [];
      }
      continue;
    }
    if (currentPath !== null) currentContent.push(line);
  }
  if (currentPath !== null && currentContent.length > 0) {
    artifacts.push({ filePath: currentPath, content: stripOuterFence(currentContent.join("\n")) });
    warnings.push(`artifact "${currentPath}" is unterminated — missing ### END marker`);
  }
  return { artifacts, strictTermination: currentPath === null, warnings };
}

// ── JSONL artifact protocol (stub) ─────────────────────────────────────────

export interface JsonlArtifact {
  type: "file_write";
  path: string;
  sha256: string;
  content_b64: string;
}

export function parseJsonlArtifacts(output: string): ParseResult {
  // JSONL-first protocol: parse each line as JSON, validate sha256 atomically
  const lines = output.split("\n");
  const jsonlArtifacts: ParsedArtifact[] = [];
  let strictTermination = true;
  const warnings: string[] = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.trim();
    if (!line) continue;
    if (line.startsWith("{")) {
      try {
        const obj = JSON.parse(line);
        if (obj && obj.type === "file_write" && typeof obj.path === "string") {
          // Decode content: prefer canonical content_b64, accept legacy content
          let fileContent: string;
          if (typeof obj.content_b64 === "string") {
            // Validate canonical base64 before decoding
            if (!/^[A-Za-z0-9+\/]*={0,2}$/.test(obj.content_b64)) {
              warnings.push(
                `line ${i + 1}: JSONL artifact "${obj.path}" has invalid base64 content_b64`,
              );
              strictTermination = false;
              continue;
            }

            fileContent = Buffer.from(obj.content_b64, "base64").toString("utf-8");
          } else if (
            typeof obj.content === "string" &&
            process.env.ALLOW_LEGACY_JSONL_CONTENT === "1"
          ) {
            fileContent = obj.content;
            warnings.push(
              `line ${i + 1}: JSONL artifact "${obj.path}" uses deprecated "content" field — prefer "content_b64"`,
            );
          } else if (typeof obj.content === "string") {
            warnings.push(
              `line ${i + 1}: JSONL artifact "${obj.path}" uses deprecated "content" field — set ALLOW_LEGACY_JSONL_CONTENT=1 to accept legacy format`,
            );
            strictTermination = false;
            continue;
          } else {
            warnings.push(
              `line ${i + 1}: JSONL artifact "${obj.path}" missing both content_b64 and content fields`,
            );
            strictTermination = false;
            continue;
          }

          // sha256 is REQUIRED for JSONL protocol integrity
          if (typeof obj.sha256 !== "string" || obj.sha256.length === 0) {
            warnings.push(`line ${i + 1}: JSONL artifact "${obj.path}" missing required sha256`);
            strictTermination = false;
            continue;
          }

          // Hard-fail on hash mismatch — no silent acceptance
          const actualHash = sha256Of(fileContent);
          if (actualHash !== obj.sha256) {
            warnings.push(
              `line ${i + 1}: JSONL sha256 mismatch for "${obj.path}": expected ${obj.sha256}, got ${actualHash}`,
            );
            strictTermination = false;
            continue;
          }

          jsonlArtifacts.push({ filePath: obj.path.trim(), content: fileContent });
        } else if (obj && typeof obj === "object" && "type" in obj) {
          warnings.push(
            `JSONL line ${i + 1}: unknown artifact type "${String(obj.type)}" — only "file_write" is recognized`,
          );
          strictTermination = false;
        }
      } catch {
        // Non-JSON line in JSONL stream — protocol violation
        warnings.push(`JSONL line ${i + 1}: not valid JSON — protocol integrity violation`);
        strictTermination = false;
      }
    } else {
      // Non-empty, non-JSON line in strict JSONL mode
      warnings.push(`JSONL line ${i + 1}: non-JSONL content in strict artifact stream`);
      strictTermination = false;
    }
  }

  if (jsonlArtifacts.length > 0) {
    return { artifacts: jsonlArtifacts, strictTermination, warnings };
  }

  // Marker protocol (### FILE: / ### END) is deprecated and no longer
  // auto-falls-back from JSONL parsing. The marker-based parser is fragile
  // with generated markdown/test/fixture content and should only be
  // used when explicitly opted into via env var.
  if (process.env.ALLOW_MARKER_ARTIFACT_FALLBACK === "1") {
    return parseArtifacts(output);
  }

  // If no artifacts were found and no JSONL lines at all, mark non-strict.
  // Empty output is also non-strict — the orchestrator's empty-emission
  // override will produce an actionable correction prompt.
  const hasAnyJsonl = output.split("\n").some((l) => l.trim().startsWith("{"));
  if (!hasAnyJsonl) {
    return {
      artifacts: [],
      strictTermination: false,
      warnings: ["No JSONL artifact protocol detected in output"],
    };
  }

  return { artifacts: [], strictTermination: true, warnings: [] };
}
// ── Internal helpers ───────────────────────────────────────────────────────

function stripOuterFence(content: string): string {
  const trimmed = content.trim();
  const fenced = trimmed.match(/^```[A-Za-z0-9_+#.-]*\s*\n([\s\S]*?)\n?```$/);
  if (fenced) return fenced[1]!.trimEnd() + "\n";
  return content.trimEnd() + "\n";
}

// ── Re-exports for backward compatibility ──────────────────────────────────

export { writeArtifacts, disallowedArtifact, segmentsHaveEscapingSymlink, finalFileIsSymlink };
export type { WriteResult };

// ── Protocol integrity ─────────────────────────────────────────────────────

/**
 * When protocol is broken (unterminated markers or truncated model output),
 * no artifact writes should reach disk. All artifacts are treated as blocked.
 */
function blockedByProtocol(artifacts: ParsedArtifact[], reason: string): WriteResult[] {
  return artifacts.map((a) => ({
    filePath: a.filePath,
    bytes: 0,
    ok: false as const,
    blocked: reason,
  }));
}

// ── Execution ──────────────────────────────────────────────────────────────

export async function executeArtifact(
  agent: Agent,
  brief: TaskBrief,
  taskId: string,
  cwd: string,
  profile?: TaskProfile,
): Promise<OrchestratorResult> {
  const result = await agent.execute(brief);
  if (!result.ok) return err(result.error);
  const emission = result.value;
  const { artifacts, strictTermination, warnings } = parseJsonlArtifacts(emission.content);
  const wasTruncated = emission.finishReason === "length" || emission.finishReason === "max_tokens";
  const protocolBroken = !strictTermination || wasTruncated;

  // Protocol integrity: broken protocol blocks ALL artifact writes
  const protocolReason = [
    ...(!strictTermination ? ["unterminated artifact block"] : []),
    ...(wasTruncated ? [`truncated model output (finish_reason: ${emission.finishReason})`] : []),
  ].join(" + ");
  const writes: WriteResult[] = protocolBroken
    ? blockedByProtocol(
        artifacts,
        warnings.length > 0
          ? `${protocolReason} — parse warnings: ${warnings.slice(0, 5).join("; ")}`
          : protocolReason,
      )
    : writeArtifacts(artifacts, cwd, profile);

  const allWarnings = [...warnings];
  if (wasTruncated) {
    allWarnings.push(
      `model output was truncated (finish_reason: ${emission.finishReason}) — artifact content may be incomplete`,
    );
  }
  if (protocolBroken && artifacts.length > 0) {
    allWarnings.push(
      `protocol integrity violation: all ${artifacts.length} artifact(s) blocked from write`,
    );
  }

  const okWrites = writes.filter((w) => w.ok);
  const blocked = writes.filter((w) => w.blocked);
  for (const w of writes) {
    if (w.warning) allWarnings.push(w.warning);
  }

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
      id: `sig-artifact-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.emitted",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: {
        filesWritten: okWrites.length,
        totalBytes: writes.reduce((s, w) => s + w.bytes, 0),
        files: okWrites.map((w) => ({
          path: w.filePath,
          sha256: w.sha256!,
          bytes: w.bytes,
          beforeHash: w.beforeHash ?? null,
          existed: w.existed ?? false,
        })),
        language: profile?.language ?? "unknown",
      },
      confidence: writes.every((w) => w.ok) ? 0.9 : 0.4,
    },
  ];
  if (!strictTermination) {
    signals.push({
      id: `sig-unterminated-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.unterminated",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { warnings: allWarnings },
    });
  }
  if (wasTruncated) {
    signals.push({
      id: `sig-truncated-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.truncated",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: { finishReason: emission.finishReason, warnings: allWarnings },
    });
  }
  if (blocked.length > 0) {
    signals.push({
      id: `sig-blocked-${taskId}`,
      taskId,
      domain: "code",
      kind: "artifact.blocked",
      source: emission.agentId,
      ts: new Date().toISOString(),
      value: {
        blockedPaths: blocked.map((b) => ({ path: b.filePath, reason: b.blocked! })),
        parseWarnings: allWarnings,
      },
    });
  }
  const dr: DelegationResult = {
    decision: {
      mode: "artifact",
      reason: `artifact emission: ${okWrites.length} files written${blocked.length > 0 ? `, ${blocked.length} blocked` : ""}${!strictTermination ? " (unterminated)" : ""}${wasTruncated ? " (truncated)" : ""}`,
      autoRouted: true,
    },
    emission,
    signals,
  };
  return ok(dr);
}
