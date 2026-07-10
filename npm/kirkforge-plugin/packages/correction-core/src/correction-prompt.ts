import type { ReducedStatePacket } from "./types.js";
import type { TaskLanguage } from "./task-language.js";

/**
 * Returns the tool names used in correction prompts for each language.
 * After v8.5 all lint/security is handled by KirkForge native engines —
 * only type-checking remains an external tool dependency (tsc/pyright).
 */
export function toolNames(language?: TaskLanguage): {
  lint: string;
  types: string;
  security: string;
} {
  switch (language) {
    case "python":
      return {
        lint: "KirkForge Python lint engine",
        types: "pyright",
        security: "KirkForge Python lint engine (safety rules)",
      };
    case "typescript":
    case "javascript":
      return {
        lint: "KirkForge TypeScript lint engine",
        types: "tsc",
        security: "KirkForge TypeScript lint engine (safety rules)",
      };
    case "shell":
      return {
        lint: "KirkForge shell lint engine",
        types: "bash -n",
        security: "KirkForge shell lint engine (safety rules)",
      };
    case "cpp":
    case "c":
      return {
        lint: "KirkForge C/C++ lint engine",
        types: "gcc/g++ -fsyntax-only",
        security: "KirkForge C/C++ lint engine (safety rules)",
      };
    case "rust":
      return {
        lint: "KirkForge Rust lint engine",
        types: "rustc --emit=metadata",
        security: "KirkForge Rust lint engine (safety rules)",
      };
    case "go":
      return {
        lint: "KirkForge Go lint engine",
        types: "go vet",
        security: "KirkForge Go lint engine (safety rules)",
      };
    case "sql":
      return {
        lint: "KirkForge SQL lint engine",
        types: "database validator",
        security: "KirkForge SQL lint engine (safety rules)",
      };
    default:
      return { lint: "lint", types: "type-check", security: "security scanner" };
  }
}

export function buildCorrectionPrompt(packet: ReducedStatePacket, language?: TaskLanguage): string {
  const tools = toolNames(language);
  const issues: string[] = [];
  if (packet.artifactEnforcement?.status === "fail") {
    if (packet.artifactEnforcement.blockedPaths.length > 0) {
      const blocked = packet.artifactEnforcement.blockedPaths
        .map((b) => `${b.path}: ${b.reason}`)
        .join("; ");
      const warnDetails = packet.artifactEnforcement.parseWarnings?.length
        ? ` Parse warnings (line numbers): ${packet.artifactEnforcement.parseWarnings.map((w) => `L${w.line}: ${w.warning}`).join("; ")}.`
        : "";
      issues.push(
        `Artifact policy blocked: ${blocked}.${warnDetails} You must emit files using JSONL protocol: {"type":"file_write","path":"...","sha256":"...","content_b64":"..."} with allowed extensions only.`,
      );
    } else {
      issues.push(
        'Artifact enforcement failed: zero files were emitted. You must produce at least one file using the JSONL protocol: {"type":"file_write","path":"...","sha256":"...","content_b64":"..."}.',
      );
    }
  }
  if (
    packet.verifierPolicy &&
    (packet.verifierPolicy.missingRequired.length > 0 ||
      packet.verifierPolicy.skippedRequired.length > 0)
  ) {
    const missing = packet.verifierPolicy.missingRequired;
    const skipped = packet.verifierPolicy.skippedRequired;
    const parts: string[] = [];
    if (missing.length > 0) parts.push(`missing: ${missing.join(", ")}`);
    if (skipped.length > 0) parts.push(`skipped: ${skipped.join(", ")}`);
    issues.push(
      `Required verifier ${parts.join("; ")}. These must run and pass for a clean result.`,
    );
  }
  if (packet.verification.lint.errors > 0) {
    issues.push(
      `Fix ${packet.verification.lint.errors} lint errors. The orchestrator will re-run ${tools.lint}.`,
    );
  }
  if (packet.verification.types.errors > 0) {
    issues.push(
      `Fix ${packet.verification.types.errors} type errors. The orchestrator will re-run ${tools.types}.`,
    );
  }
  if (packet.verification.security.critical > 0) {
    issues.push(
      `Address ${packet.verification.security.critical} critical security findings. The orchestrator will re-run ${tools.security}.`,
    );
  }
  if (packet.verification.security.high > 0) {
    issues.push(
      `Address ${packet.verification.security.high} high-severity security findings. The orchestrator will re-run ${tools.security}.`,
    );
  }
  if (packet.graph.brokenEdges > 0) {
    const graphIsAdvisory = packet.verifierPolicy
      ? !packet.verifierPolicy.required.includes("graph") &&
        packet.verifierPolicy.advisory.includes("graph")
      : false;
    if (graphIsAdvisory) {
      issues.push(`Graph advisory finding: ${packet.graph.brokenEdges} broken import edges.`);
    } else {
      issues.push(`Fix ${packet.graph.brokenEdges} broken import edges.`);
    }
  }
  if (issues.length === 0 && packet.verification.overall === "fail") {
    issues.push(
      "Verification failed with no specific error details. Your output may have produced no usable files or violated protocol requirements. Ensure you emit at least one valid file using the correct protocol for this delegation mode.",
    );
  }
  return [
    "Your previous output didn't pass verification. Make targeted fixes only -- do not rewrite the file.",
    "",
    ...issues,
    "",
    `Files you changed: ${packet.changes.paths.join(", ") || "unknown"}`,
    "",
    "Output the corrected version of just those files. Verification battery will re-run.",
  ].join("\n");
}
