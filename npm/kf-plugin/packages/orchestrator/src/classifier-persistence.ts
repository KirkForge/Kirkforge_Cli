/**
 * Classifier persistence layer.
 *
 * Extends the TF-IDF classifier so it learns from actual task outcomes.
 * Instead of relying solely on hardcoded archetypes, the classifier
 * incorporates real task descriptions + their outcomes to continuously
 * improve routing accuracy.
 */
import { resetNlpModel } from "./classifier-nlp.js";
import type { DelegationMode } from "@kirkforge/core-types";
import type { MemoryStore } from "@kirkforge/memory-palace";

// ---------------------------------------------------------------------------
// Outcome weights
// ---------------------------------------------------------------------------

/** Weight applied to each outcome class for classifier learning. */
const OUTCOME_WEIGHTS: Record<string, number> = {
  pass: 1.0,
  task_fail: -0.3, // slight negative — the mode was wrong for this task
  validator_error: -0.1, // tiny negative — infrastructure issue, not mode
  tool_error: -0.1,
  escalated: 0.0, // neutral — escalated for other reasons
  unknown: 0.0,
};

// ---------------------------------------------------------------------------
// ClassifierMemory
// ---------------------------------------------------------------------------

interface LearnedExample {
  tokens: string[];
  mode: DelegationMode;
  weight: number;
  timestamp: string;
}

export class ClassifierMemory {
  private learned: LearnedExample[] = [];
  private store: MemoryStore | null = null;
  private loadedFromStore = false;

  constructor(store?: MemoryStore) {
    this.store = store ?? null;
  }

  /**
   * Learn from a single task outcome. Positive outcomes reinforce the mode;
   * negative outcomes provide a mild corrective signal.
   */
  learn(description: string, mode: DelegationMode, outcomeClass: string, weight?: number): void {
    const w = weight ?? OUTCOME_WEIGHTS[outcomeClass] ?? 0;
    if (w === 0) return;

    const tokens = this._tokenize(description);
    if (tokens.length < 2) return;

    this.learned.push({
      tokens,
      mode,
      weight: w,
      timestamp: new Date().toISOString(),
    });

    // Keep max 1000 learned examples to bound memory
    if (this.learned.length > 1000) {
      this.learned = this.learned.slice(-1000);
    }

    // Invalidate NLP model cache so next classify uses updated data
    resetNlpModel();
  }

  /**
   * Load historical outcomes from the MemoryStore and incorporate them
   * into the classifier's learned model.
   */
  async loadFromStore(): Promise<number> {
    if (!this.store) return 0;
    if (this.loadedFromStore) return this.learned.length;

    try {
      const result = await this.store.adapter.query({
        kind: "task-observation",
        limit: 500,
      });
      if (!result.ok || !result.value) return 0;

      let loaded = 0;
      for (const obs of result.value) {
        const description = String(obs.properties.description ?? obs.description);
        const mode = String(obs.properties.mode ?? "hard-prompt") as DelegationMode;
        const outcomeClass = String(
          obs.properties.outcomeClass ?? obs.properties.outcome ?? "unknown",
        );

        // Only learn from tasks with clear pass/fail outcomes
        const weight = OUTCOME_WEIGHTS[outcomeClass] ?? 0;
        if (weight === 0) continue;

        const tokens = this._tokenize(description);
        if (tokens.length < 2) continue;

        this.learned.push({
          tokens,
          mode,
          weight,
          timestamp: obs.timestamp,
        });
        loaded++;
      }

      this.loadedFromStore = true;
      if (loaded > 0) resetNlpModel();
      return loaded;
    } catch {
      return 0;
    }
  }

  /**
   * Get learned examples for NLP model building.
   * Positive examples are included as-is; negative examples are excluded
   * from centroid computation (they only serve to reduce confidence).
   */
  getLearnedExamples(): LearnedExample[] {
    return this.learned;
  }

  /** Number of learned examples currently held. */
  get size(): number {
    return this.learned.length;
  }

  /** Clear all learned examples. */
  reset(): void {
    this.learned = [];
    this.loadedFromStore = false;
    resetNlpModel();
  }

  private _tokenize(text: string): string[] {
    return text
      .toLowerCase()
      .replace(/[^a-z0-9\s]/g, " ")
      .split(/\s+/)
      .filter((t) => t.length > 1);
  }
}
