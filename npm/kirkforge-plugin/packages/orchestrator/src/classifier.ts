import type { TaskInput } from "./types.js";
import type { DelegationDecision, DelegationMode } from "@kirkforge/core-types";
import { classifyHybrid } from "./classifier-nlp.js";
import type { ClassifierMemory } from "./classifier-persistence.js";

const MODE_SCORING: Array<{
  pattern: RegExp;
  mode: DelegationMode;
  score: number;
  reason: string;
}> = [
  // artifact wins strongly for explicit file-creation language
  {
    pattern:
      /\b(?:generate|create|write|build|make)\s+(?:a\s+)?(?:\w+\s+)?(?:file|component|module|service|class|server|app|script)\b/i,
    score: 20,
    mode: "artifact",
    reason: "file creation task",
  },
  {
    pattern: /\b(?:file|files|write to|save to)\b/i,
    score: 10,
    mode: "artifact",
    reason: "file output task",
  },
  // schema-contract for structured/audit work
  {
    pattern: /\b(?:structured response|json schema|contract format|audit report)\b/i,
    score: 15,
    mode: "schema-contract",
    reason: "structured/contract task",
  },
  {
    pattern: /\b(?:audit|assess|evaluate|review\s+(?:the|this))\b/i,
    score: 8,
    mode: "schema-contract",
    reason: "analysis/audit task",
  },
  {
    pattern: /\b(?:validate|verify)\b/i,
    score: 5,
    mode: "schema-contract",
    reason: "validation task",
  },
  // hard-prompt for repairs/fixes
  // task-decompose for multi-step/pipeline work
  {
    pattern:
      /\b(?:full-stack|end-to-end|multi-step|pipeline|workflow|build a (?:complete|full|whole)|from scratch|boilerplate|scaffold)\b/i,
    score: 25,
    mode: "task-decompose",
    reason: "multi-step/pipeline task",
  },
  {
    pattern:
      /\b(?:break (?:down|into|this)|decompose|subtasks|step (?:by step|1)|plan (?:out|the))\b/i,
    score: 20,
    mode: "task-decompose",
    reason: "explicit decomposition request",
  },
  {
    pattern: /\b(?:fix|lint error|repair|refactor)\b/i,
    score: 5,
    mode: "hard-prompt",
    reason: "repair/fix task",
  },
];

function classifyByScoring(description: string): {
  mode: DelegationMode;
  reason: string;
  confidence: number;
} {
  const scores: Partial<Record<DelegationMode, number>> = {};
  let bestReason = "default";

  for (const p of MODE_SCORING) {
    if (p.pattern.test(description)) {
      scores[p.mode] = (scores[p.mode] ?? 0) + p.score;
      bestReason = p.reason;
    }
  }

  let best: DelegationMode = "hard-prompt";
  let highest = 0;
  for (const m of ["artifact", "schema-contract", "hard-prompt", "task-decompose"] as const) {
    if ((scores[m] ?? 0) > highest) {
      best = m;
      highest = scores[m]!;
    }
  }

  // artifact wins ties — code-gen is the dominant use case
  // task-decompose wins over artifact for multi-step work
  const art = scores["artifact"] ?? 0;
  const tc = scores["schema-contract"] ?? 0;
  const td = scores["task-decompose"] ?? 0;
  if (td > 0 && td >= art && td >= tc) {
    best = "task-decompose";
    bestReason = "multi-step decomposition (overrides code-gen)";
  } else if (art > 0 && art >= tc) {
    best = "artifact";
    bestReason = "file creation" + (tc > 0 ? " (overrides audit)" : "");
  }

  // Compute confidence from score margin
  const secondHighest = Math.max(
    ...(["artifact", "schema-contract", "hard-prompt", "task-decompose"] as const)
      .filter((m) => m !== best)
      .map((m) => scores[m] ?? 0),
  );
  const margin = highest - secondHighest;
  const confidence = Math.min(0.9, (margin / Math.max(1, highest)) * Math.min(1, highest / 20));

  return { mode: best, reason: bestReason, confidence };
}

const NLP_FALLBACK_THRESHOLD = 0.35;

export function classifyTask(
  task: TaskInput,
  classifierMemory?: ClassifierMemory | null,
): DelegationDecision {
  if (task.modeOverride) {
    return { mode: task.modeOverride, reason: "user override", autoRouted: false };
  }

  const regexResult = classifyByScoring(task.description);

  // Use regex result if confidence is high enough
  if (regexResult.confidence >= NLP_FALLBACK_THRESHOLD) {
    return {
      mode: regexResult.mode,
      reason: regexResult.reason,
      autoRouted: regexResult.mode !== "hard-prompt" || regexResult.confidence > 0.1,
    };
  }

  // Low confidence — fall back to NLP/TF-IDF classifier
  const nlpResult = classifyHybrid(task.description, classifierMemory);
  return {
    mode: nlpResult.mode,
    reason: `nlp-classified (regex confidence ${regexResult.confidence.toFixed(2)} < ${NLP_FALLBACK_THRESHOLD}, nlp confidence ${nlpResult.confidence.toFixed(2)})`,
    autoRouted: true,
  };
}

// Re-export for testing
export { classifyHybrid, classifyNlp } from "./classifier-nlp.js";
export { ClassifierMemory } from "./classifier-persistence.js";
export { resetNlpModel } from "./classifier-nlp.js";
