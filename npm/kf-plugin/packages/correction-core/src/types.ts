export type VerifierSlot = "lint" | "types" | "security" | "graph" | "imports";

export interface VerifierPolicy {
  required: VerifierSlot[];
  advisory: VerifierSlot[];
}

export interface VerifierPolicyResult {
  required: VerifierSlot[];
  advisory: VerifierSlot[];
  missingRequired: VerifierSlot[];
  skippedRequired: VerifierSlot[];
}

export interface ArtifactEnforcement {
  blocked: number;
  blockedPaths: Array<{ path: string; reason: string }>;
  reason?: string;
  status: "pass" | "fail";
  parseWarnings?: Array<{ line: number; warning: string }>;
  unterminated?: boolean;
  unterminatedWarnings?: string[];
  truncated?: boolean;
  truncatedFinishReason?: string;
  truncatedWarnings?: string[];
}

export interface ReducedStatePacket {
  taskId: string;
  turn: number;
  ts: string;
  driftScore?: number;
  changes: { filesChanged: number; paths: string[]; insertions: number; deletions: number };
  graph: { edgeCount: number; newEdges: number; brokenEdges: number; cycles: number };
  verification: {
    lint: { errors: number; warnings: number; suppressed?: number };
    types: { errors: number };
    security: { findings: number; critical: number; high: number };
    imports?: { findings: number; warnings: number; info: number };
    overall: "pass" | "warn" | "fail";
  };
  artifactEnforcement?: ArtifactEnforcement;
  emissions?: {
    filesWritten: number;
    totalBytes: number;
    files: Array<{
      path: string;
      sha256: string;
      bytes: number;
      beforeHash: string | null;
      existed: boolean;
    }>;
  };
  verifierPolicy?: VerifierPolicyResult;
  contributingSignals: Array<{ kind: string; ts: string; source: string }>;
  /** Hash of the active policy at the time this packet was produced. Used for audit trail verification. */
  policyHash?: string;
}

export interface CorrectionConfig {
  maxCorrections: number;
  maxCost?: number;
}

export interface CorrectionDecision {
  action: "accept" | "correct" | "escalate";
  rationale: string;
  correctionPrompt?: string;
  packet: ReducedStatePacket;
  correctionCount: number;
  workerTokens: number;
  sessionTokens: number;
}
