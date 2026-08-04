import type { MemoryObject, Recommendation, RoutingCase } from "./types.js";

/**
 * Snake-case → camelCase mapper for SQLite rows. Picks either the
 * snake_case or camelCase form for each field, preferring the snake_case
 * shape produced by the SQLite adapter.
 */
export function rowToProperties(r: Record<string, unknown>): Record<string, unknown> {
  return {
    language: r.language,
    taskFamily: r.task_family ?? r.taskFamily,
    mode: r.mode,
    model: r.model,
    providerKey: r.provider_key ?? r.providerKey,
    providerType: r.provider_type ?? r.providerType,
    baseUrl: r.base_url ?? r.baseUrl,
    outcome: r.outcome,
    outcomeClass: r.outcome_class ?? r.outcomeClass,
    routingLesson: r.routing_lesson ?? r.routingLesson,
    finalVerdict: r.final_verdict ?? r.finalVerdict,
    sourceOfTruth: r.source_of_truth ?? r.sourceOfTruth,
    finalAction: r.final_action ?? r.finalAction,
    tokens: r.tokens,
    durationMs: r.duration_ms ?? r.durationMs,
    turns: r.turns,
    validatorDurationMs: r.validator_duration_ms ?? r.validatorDurationMs,
    verifierOverall: r.verifier_overall ?? r.verifierOverall,
    filesEmitted: r.files_emitted ?? r.filesEmitted,
    totalBytesEmitted: r.total_bytes_emitted ?? r.totalBytesEmitted,
    emissionCount: r.emissionCount,
    emissionIds: r.emission_ids ?? r.emissionIds ?? [],
  };
}

/** Regex-based coarse task-family classifier. */
export function detectFamily(description: string): string {
  const lower = description.toLowerCase();
  if (/web|http|server|endpoint|api/.test(lower)) return "web";
  if (/script|cli|command|shell/.test(lower)) return "script";
  if (/test|spec|verify|check/.test(lower)) return "testing";
  if (/data|parse|scrape|csv|json/.test(lower)) return "data";
  if (/fix|debug|repair|patch/.test(lower)) return "debugging";
  return "general";
}

/** Stopword-filtered tokenizer. Capped at 40 unique tokens. */
export function tokenize(text: string): string[] {
  const stop = new Set([
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "into",
    "using",
    "task",
    "file",
    "files",
    "write",
    "create",
    "build",
    "make",
  ]);
  return [...new Set(text.toLowerCase().match(/[a-z0-9][a-z0-9._-]{2,}/g) ?? [])]
    .filter((word) => !stop.has(word))
    .slice(0, 40);
}

/** FNV-1a-hashed bag-of-words vector. Default 64 dimensions. */
export function vectorize(tokens: string[], dimensions = 64): number[] {
  const vector = Array.from({ length: dimensions }, () => 0);
  for (const token of tokens) {
    let hash = 2166136261;
    for (let i = 0; i < token.length; i++) {
      hash ^= token.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    vector[Math.abs(hash) % dimensions]! += 1;
  }
  return vector;
}

/** Cosine similarity. Zero-vectors return 0. */
export function cosine(a: number[], b: number[]): number {
  let dot = 0,
    an = 0,
    bn = 0;
  const length = Math.max(a.length, b.length);
  for (let i = 0; i < length; i++) {
    const av = a[i] ?? 0;
    const bv = b[i] ?? 0;
    dot += av * bv;
    an += av * av;
    bn += bv * bv;
  }
  if (an === 0 || bn === 0) return 0;
  return dot / (Math.sqrt(an) * Math.sqrt(bn));
}

/**
 * Build a routing recommendation from prior observations. Aggregates
 * pass/fail-weighted scores per model and per mode, applying a "truth
 * weight" (task-validator=2.0, verifier=1.0). Returns a Recommendation
 * with prefer/avoid model lists when evidence is sufficient.
 */
export function buildEmpiricalRecommendation(
  taskDescription: string,
  observations: MemoryObject[],
  workerModel?: string,
): Recommendation | null {
  const query = fingerprintTask(taskDescription, "unknown");
  const similar = observations
    .map((object) => {
      const vector = Array.isArray(object.properties.vector)
        ? (object.properties.vector as number[])
        : vectorize([
            String(object.properties.language ?? ""),
            String(object.properties.taskFamily ?? ""),
            ...tokenize(object.description),
          ]);
      const similarity = cosine(query.vector, vector);
      const sameFamily = object.properties.taskFamily === query.taskFamily ? 0.25 : 0;
      return { object, similarity: Math.min(1, similarity + sameFamily) };
    })
    .filter((entry) => entry.similarity >= 0.25)
    .sort((a, b) => b.similarity - a.similarity)
    .slice(0, 12);

  if (similar.length === 0) return null;

  const byModel = new Map<string, { pass: number; fail: number; tokens: number; score: number }>();
  const byMode = new Map<string, { pass: number; fail: number; score: number }>();
  const cases: RoutingCase[] = [];

  for (const entry of similar) {
    const p = entry.object.properties;
    const model = String(p.model ?? "unknown");
    const mode = String(p.mode ?? "hard-prompt");
    const outcome = normalizeOutcome(p.outcome);
    const sourceOfTruth = String(p.sourceOfTruth ?? "verifier");
    const routingLessonRaw = String(p.routingLesson ?? "");
    // Derive routingLesson from outcome when not explicitly set
    const routingLesson = routingLessonRaw
      ? routingLessonRaw
      : outcome === "pass"
        ? "reward"
        : outcome === "fail"
          ? "punish"
          : "neutral";
    const truthFactor = sourceOfTruth === "task-validator" ? 2.0 : 1.0;
    const weight = entry.similarity * truthFactor;
    const modelStats = byModel.get(model) ?? { pass: 0, fail: 0, tokens: 0, score: 0 };
    const modeStats = byMode.get(mode) ?? { pass: 0, fail: 0, score: 0 };
    // Use routingLesson for scoring when available, fall back to outcome
    if (routingLesson === "reward") {
      modelStats.pass += weight;
      modeStats.pass += weight;
    } else if (routingLesson === "punish") {
      modelStats.fail += weight;
      modeStats.fail += weight;
    } else if (routingLesson === "neutral") {
      // neutral — do not score, just track
    } else if (outcome === "pass") {
      modelStats.pass += weight;
      modeStats.pass += weight;
    } else if (outcome === "fail") {
      modelStats.fail += weight;
      modeStats.fail += weight;
    }
    // "error" outcomes (infra failures, escalations, unknowns) are excluded
    // from pass/fail counts so they do not punish or reward a model for
    // circumstances outside its control.
    modelStats.tokens += Number(p.tokens ?? 0) * weight;
    modelStats.score += weight;
    modeStats.score += weight;
    byModel.set(model, modelStats);
    byMode.set(mode, modeStats);
    cases.push({
      taskFamily: String(p.taskFamily ?? "unknown"),
      language: String(p.language ?? "unknown"),
      mode,
      model,
      outcome,
      outcomeClass: String(p.outcomeClass ?? "unknown") as RoutingCase["outcomeClass"],
      sourceOfTruth: String(p.sourceOfTruth ?? "verifier") as RoutingCase["sourceOfTruth"],
      reason: String(p.reason ?? outcome),
      tokens: Number(p.tokens ?? 0),
      durationMs: Number(p.durationMs ?? 0),
      similarity: Number(entry.similarity.toFixed(3)),
      truthWeight: sourceOfTruth === "task-validator" ? 2.0 : 1.0,
    });
  }

  const rankedModels = [...byModel.entries()]
    .map(([model, data]) => ({
      model,
      passRate: data.pass / Math.max(0.001, data.pass + data.fail),
      evidence: data.pass + data.fail,
      expectedTokens: Math.round(data.tokens / Math.max(0.001, data.score)),
    }))
    .sort((a, b) => b.passRate - a.passRate || b.evidence - a.evidence);
  const rankedModes = [...byMode.entries()]
    .map(([mode, data]) => ({
      mode,
      passRate: data.pass / Math.max(0.001, data.pass + data.fail),
      evidence: data.pass + data.fail,
    }))
    .sort((a, b) => b.passRate - a.passRate || b.evidence - a.evidence);

  const prefer = rankedModels
    .filter((m) => m.passRate >= 0.62)
    .slice(0, 2)
    .map((m) => m.model);
  const avoid = rankedModels
    .filter((m) => m.passRate <= 0.38 && m.evidence >= 0.35)
    .slice(0, 3)
    .map((m) => m.model);
  const bestModel = prefer[0] ?? rankedModels[0]?.model ?? workerModel ?? "unknown";
  const bestMode = rankedModes[0]?.mode ?? "hard-prompt";
  const bestModelStats = rankedModels.find((m) => m.model === bestModel);
  const evidence = similar.length;
  const confidence = Math.min(
    0.9,
    (bestModelStats?.evidence ?? evidence) / ((bestModelStats?.evidence ?? evidence) + 2),
  );

  return {
    mode: bestMode,
    model: workerModel ?? bestModel,
    confidence,
    evidence,
    expectedTokens: bestModelStats?.expectedTokens ?? 0,
    score: rankedModes[0]?.passRate ?? 0,
    routingBias: {
      prefer,
      avoid,
      confidence,
      influence: 0.25,
      evidence,
      similarCases: cases.slice(0, 5),
    },
  };
}

/** Wraps tokenize + vectorize + detectFamily. */
export function fingerprintTask(description: string, _defaultFamily: string) {
  const tokens = tokenize(description);
  const vector = vectorize(tokens);
  const taskFamily = detectFamily(description);
  return { tokens, vector, taskFamily };
}

// eslint-disable-next-line @typescript-eslint/no-unused-vars
function normalizeOutcomeClass(
  value: unknown,
): "pass" | "task_fail" | "validator_error" | "tool_error" | "escalated" | "unknown" {
  const valid = ["pass", "task_fail", "validator_error", "tool_error", "escalated", "unknown"];
  return typeof value === "string" && valid.includes(value)
    ? (value as "pass" | "task_fail" | "validator_error" | "tool_error" | "escalated" | "unknown")
    : "unknown";
}

/** Coerce unknown values into one of the three valid outcomes. Defaults to "error". */
export function normalizeOutcome(value: unknown): "pass" | "fail" | "error" {
  return value === "pass" || value === "fail" || value === "error" ? value : "error";
}
