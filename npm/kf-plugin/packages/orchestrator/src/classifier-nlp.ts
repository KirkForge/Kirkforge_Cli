/**
 * TF-IDF based task classifier for KirkForge.
 *
 * Uses a lightweight term-frequency / inverse-document-frequency vectorizer
 * to classify task descriptions into delegation modes (artifact, schema-contract,
 * hard-prompt). This complements the regex-based classifier for ambiguous inputs.
 *
 * Design: zero dependencies beyond Node built-ins. Vocabulary built from
 * curated task archetypes. Cosine similarity against archetype centroids.
 */

import type { DelegationMode } from "@kirkforge/core-types";
import type { ClassifierMemory } from "./classifier-persistence.js";

// ── Task archetypes ────────────────────────────────────────────────────────

interface Archetype {
  mode: DelegationMode;
  examples: string[];
}

const ARCHETYPES: Archetype[] = [
  {
    mode: "artifact",
    examples: [
      "create a new react component file",
      "generate a python module for data processing",
      "write a typescript utility function",
      "build a REST API endpoint handler",
      "make a configuration file for deployment",
      "implement a database migration script",
      "scaffold a new service module",
      "generate unit tests for the auth module",
      "write a Dockerfile for the application",
      "create a bash deployment script",
      "build a CSS stylesheet for the dashboard",
      "generate HTML template for email notifications",
      "write a SQL migration for adding columns",
      "implement a go middleware handler",
      "create a rust library module",
    ],
  },
  {
    mode: "schema-contract",
    examples: [
      "audit the codebase for security vulnerabilities",
      "assess the architecture for scalability issues",
      "evaluate the test coverage gaps",
      "review the pull request for merge readiness",
      "validate the input configuration against schema",
      "verify the deployment manifests are correct",
      "analyze the dependency tree for outdated packages",
      "inspect the logging configuration for completeness",
      "check all environment variables are documented",
      "examine the error handling patterns",
      "summarize the changes in this release",
      "document the API endpoints with OpenAPI spec",
      "compare the two implementations for correctness",
    ],
  },
  {
    mode: "hard-prompt",
    examples: [
      "fix the lint errors in the auth module",
      "repair the broken type definitions",
      "refactor the database access layer",
      "optimize the slow query performance",
      "debug the authentication middleware",
      "resolve the merge conflicts in main branch",
      "patch the security vulnerability in dependencies",
      "correct the import path references",
      "update the deprecated API calls",
      "simplify the complex reducer logic",
      "clean up unused variables and imports",
      "migrate from old API to new API version",
      "troubleshoot the failing integration tests",
      "tighten the type annotations across the module",
    ],
  },
];

// ── Tokenizer ──────────────────────────────────────────────────────────────

function tokenize(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, " ")
    .split(/\s+/)
    .filter((t) => t.length > 1 && !STOP_WORDS.has(t));
}

const STOP_WORDS = new Set([
  "the",
  "a",
  "an",
  "is",
  "are",
  "was",
  "were",
  "be",
  "been",
  "being",
  "have",
  "has",
  "had",
  "do",
  "does",
  "did",
  "will",
  "would",
  "could",
  "should",
  "may",
  "might",
  "can",
  "shall",
  "to",
  "of",
  "in",
  "for",
  "on",
  "with",
  "at",
  "by",
  "from",
  "as",
  "into",
  "through",
  "during",
  "before",
  "after",
  "above",
  "below",
  "between",
  "out",
  "off",
  "over",
  "under",
  "again",
  "further",
  "then",
  "once",
  "here",
  "there",
  "when",
  "where",
  "why",
  "how",
  "all",
  "both",
  "each",
  "few",
  "more",
  "most",
  "other",
  "some",
  "such",
  "no",
  "nor",
  "not",
  "only",
  "own",
  "same",
  "so",
  "than",
  "too",
  "very",
  "just",
  "about",
  "and",
  "but",
  "or",
  "it",
  "its",
  "this",
  "that",
  "these",
  "those",
]);

// ── TF-IDF vectorizer ──────────────────────────────────────────────────────

interface TfIdfModel {
  vocabulary: string[];
  idf: Map<string, number>;
  centroids: Map<DelegationMode, Float64Array>;
}

function computeTf(doc: string[]): Map<string, number> {
  const tf = new Map<string, number>();
  for (const token of doc) {
    tf.set(token, (tf.get(token) ?? 0) + 1);
  }
  const total = doc.length || 1;
  for (const [k, v] of tf) {
    tf.set(k, v / total);
  }
  return tf;
}

function buildVocabulary(docs: string[][]): { vocabulary: string[]; idf: Map<string, number> } {
  const df = new Map<string, number>();
  const N = docs.length;

  for (const doc of docs) {
    const seen = new Set(doc);
    for (const token of seen) {
      df.set(token, (df.get(token) ?? 0) + 1);
    }
  }

  const vocabulary = [...df.keys()].sort();
  const idf = new Map<string, number>();
  for (const [token, count] of df) {
    idf.set(token, Math.log((N + 1) / (count + 1)) + 1);
  }

  return { vocabulary, idf };
}

function vectorize(doc: string[], vocabulary: string[], idf: Map<string, number>): Float64Array {
  const tf = computeTf(doc);
  const vec = new Float64Array(vocabulary.length);
  for (let i = 0; i < vocabulary.length; i++) {
    const token = vocabulary[i]!;
    vec[i] = (tf.get(token) ?? 0) * (idf.get(token) ?? 0);
  }
  return vec;
}

function cosineSimilarity(a: Float64Array, b: Float64Array): number {
  let dot = 0;
  let normA = 0;
  let normB = 0;
  for (let i = 0; i < a.length; i++) {
    dot += a[i]! * b[i]!;
    normA += a[i]! ** 2;
    normB += b[i]! ** 2;
  }
  const denom = Math.sqrt(normA) * Math.sqrt(normB);
  return denom === 0 ? 0 : dot / denom;
}

// ── Model construction ─────────────────────────────────────────────────────

let _model: TfIdfModel | null = null;

function getModel(classifierMemory?: ClassifierMemory | null): TfIdfModel {
  if (_model) return _model;

  const allDocs: string[][] = [];
  const modeDocs = new Map<DelegationMode, string[][]>();

  for (const archetype of ARCHETYPES) {
    const docs = archetype.examples.map(tokenize);
    allDocs.push(...docs);
    modeDocs.set(archetype.mode, docs);
  }

  // Incorporate learned examples from actual outcomes
  if (classifierMemory) {
    const learned = classifierMemory.getLearnedExamples();
    for (const ex of learned) {
      if (ex.weight > 0) {
        // Positive outcome: add to mode docs for centroid computation
        const docs = modeDocs.get(ex.mode) ?? [];
        docs.push(ex.tokens);
        modeDocs.set(ex.mode, docs);
        allDocs.push(ex.tokens);
      }
    }
  }

  const { vocabulary, idf } = buildVocabulary(allDocs);

  // Compute centroid per mode
  const centroids = new Map<DelegationMode, Float64Array>();
  for (const [mode, docs] of modeDocs) {
    const vectors = docs.map((d) => vectorize(d, vocabulary, idf));
    const centroid = new Float64Array(vocabulary.length);
    for (const v of vectors) {
      for (let i = 0; i < v.length; i++) {
        centroid[i]! += v[i]!;
      }
    }
    for (let i = 0; i < centroid.length; i++) {
      centroid[i]! /= vectors.length;
    }
    centroids.set(mode, centroid);
  }

  _model = { vocabulary, idf, centroids };
  return _model;
}

// ── Classification ─────────────────────────────────────────────────────────

export interface NlpResult {
  mode: DelegationMode;
  confidence: number;
  scores: Record<DelegationMode, number>;
}

/**
 * Classify a task description using TF-IDF + cosine similarity against
 * archetype centroids. Returns the best mode with confidence score.
 */
export function classifyNlp(
  description: string,
  classifierMemory?: ClassifierMemory | null,
): NlpResult {
  const model = getModel(classifierMemory);
  const tokens = tokenize(description);
  const vec = vectorize(tokens, model.vocabulary, model.idf);

  const scores: Record<DelegationMode, number> = {
    artifact: 0,
    "schema-contract": 0,
    "hard-prompt": 0,
    "task-decompose": 0,
  };

  for (const [mode, centroid] of model.centroids) {
    scores[mode] = cosineSimilarity(vec, centroid);
  }

  let best: DelegationMode = "hard-prompt";
  let bestScore = 0;
  for (const [mode, score] of Object.entries(scores)) {
    if (score > bestScore) {
      bestScore = score;
      best = mode as DelegationMode;
    }
  }

  // Confidence: ratio of best to second-best (0 = tie, 1 = clear winner)
  const sorted = Object.values(scores).sort((a, b) => b - a);
  const margin = sorted[0]! - (sorted[1] ?? 0);
  const confidence = Math.min(1.0, Math.max(0.0, margin / Math.max(0.01, sorted[0]!)));

  return { mode: best, confidence, scores };
}

/**
 * Hybrid classifier: uses regex for clear cases, falls back to NLP
 * for ambiguous inputs. Returns mode + confidence.
 */
export function classifyHybrid(
  description: string,
  classifierMemory?: ClassifierMemory | null,
): { mode: DelegationMode; confidence: number } {
  // Quick regex check — if strongly artifact, use it
  const lower = description.toLowerCase();
  const strongArtifact =
    /\b(?:generate|create|write|build|make)\s+(?:a\s+)?(?:\w+\s+)?(?:file|component|module|service|class|server|app|script)\b/i;
  const strongAudit = /\b(?:audit|assess|evaluate|validate|verify)\b/i;
  const strongFix = /\b(?:fix|repair|refactor|debug|patch)\b/i;

  const artHit = strongArtifact.test(lower);
  const audHit = strongAudit.test(lower);
  const fixHit = strongFix.test(lower);

  // If exactly one strong signal, use it
  const signals = [artHit, audHit, fixHit].filter(Boolean).length;
  if (signals === 1) {
    if (artHit) return { mode: "artifact", confidence: 0.8 };
    if (audHit) return { mode: "schema-contract", confidence: 0.7 };
    if (fixHit) return { mode: "hard-prompt", confidence: 0.7 };
  }

  // Ambiguous: use NLP
  const nlp = classifyNlp(description, classifierMemory);
  return { mode: nlp.mode, confidence: nlp.confidence };
}

/** Reset cached model (for testing). */
export function resetNlpModel(): void {
  _model = null;
}
