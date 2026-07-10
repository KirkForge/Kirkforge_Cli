import { z } from "zod";

export const ToolSeveritySchema = z.enum(["info", "low", "medium", "high", "critical"]);

export const KirkForgeConfigSchema = z.object({
  workspace: z.string().default("."),
  orchestrator: z
    .object({
      maxConcurrentWorkers: z.number().int().positive().default(4),
      retryAttempts: z.number().int().nonnegative().default(3),
      retryDelayMs: z.number().int().positive().default(1000),
    })
    .default({
      maxConcurrentWorkers: 4,
      retryAttempts: 3,
      retryDelayMs: 1000,
    }),
  tools: z
    .object({
      eslint: z
        .object({ enabled: z.boolean().default(true), configFile: z.string().optional() })
        .default({ enabled: true }),
      secdev: z.object({ enabled: z.boolean().default(true) }).default({ enabled: true }),
      gitnexus: z.object({ enabled: z.boolean().default(true) }).default({ enabled: true }),
      graphify: z
        .object({
          enabled: z.boolean().default(false),
          queryBudget: z.number().int().positive().optional(),
        })
        .default({ enabled: false }),
    })
    .default({
      eslint: { enabled: true },
      secdev: { enabled: true },
      gitnexus: { enabled: true },
      graphify: { enabled: false },
    }),
  logging: z
    .object({
      level: z.enum(["trace", "debug", "info", "warn", "error"]).default("info"),
      format: z.enum(["json", "human"]).default("json"),
      output: z.string().optional(),
    })
    .default({ level: "info", format: "json" }),
  memory: z
    .object({
      path: z.string().default(".kirkforge/memory"),
      retentionDays: z.number().int().positive().default(30),
    })
    .default({ path: ".kirkforge/memory", retentionDays: 30 }),
});

export type KirkForgeConfig = z.infer<typeof KirkForgeConfigSchema>;

export const VerifierSlotSchema = z.enum(["lint", "types", "security", "graph"]);

export const VerifierPolicySchema = z.object({
  required: z.array(VerifierSlotSchema),
  advisory: z.array(VerifierSlotSchema),
});

export const VerifierPolicyResultSchema = z.object({
  required: z.array(VerifierSlotSchema),
  advisory: z.array(VerifierSlotSchema),
  missingRequired: z.array(VerifierSlotSchema),
  skippedRequired: z.array(VerifierSlotSchema),
});

export const ArtifactEnforcementSchema = z.object({
  blocked: z.number(),
  blockedPaths: z.array(z.object({ path: z.string(), reason: z.string() })),
  status: z.enum(["pass", "fail"]),
  unterminated: z.boolean().optional(),
  unterminatedWarnings: z.array(z.string()).optional(),
  truncated: z.boolean().optional(),
  truncatedFinishReason: z.string().optional(),
  truncatedWarnings: z.array(z.string()).optional(),
});

export const ReducedStatePacketSchema = z.object({
  taskId: z.string(),
  turn: z.number(),
  ts: z.string(),
  driftScore: z.number().optional(),
  changes: z.object({
    filesChanged: z.number(),
    paths: z.array(z.string()),
    insertions: z.number(),
    deletions: z.number(),
  }),
  graph: z.object({
    edgeCount: z.number(),
    newEdges: z.number(),
    brokenEdges: z.number(),
    cycles: z.number(),
  }),
  verification: z.object({
    lint: z.object({ errors: z.number(), warnings: z.number(), suppressed: z.number().optional() }),
    types: z.object({ errors: z.number() }),
    security: z.object({ findings: z.number(), critical: z.number(), high: z.number() }),
    overall: z.enum(["pass", "warn", "fail"]),
  }),
  artifactEnforcement: ArtifactEnforcementSchema.optional(),
  emissions: z
    .object({
      filesWritten: z.number(),
      totalBytes: z.number(),
      files: z.array(
        z.object({
          path: z.string(),
          sha256: z.string(),
          bytes: z.number(),
          beforeHash: z.string().nullable(),
          existed: z.boolean(),
        }),
      ),
    })
    .optional(),
  verifierPolicy: VerifierPolicyResultSchema.optional(),
  contributingSignals: z.array(
    z.object({
      kind: z.string(),
      ts: z.string(),
      source: z.string(),
    }),
  ),
});

export type ReducedStatePacket = z.infer<typeof ReducedStatePacketSchema>;
