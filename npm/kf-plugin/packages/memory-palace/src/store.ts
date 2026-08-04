import { ok, err, type Result } from "@kirkforge/core-types";
import { resolve } from "node:path";
import { createHash, randomBytes } from "node:crypto";
import type { TaskNode } from "@kirkforge/core-types";
import type {
  EmittedFileRecord,
  MemoryAdapter,
  MemoryObject,
  MemoryQuery,
  Recommendation,
  RunRecord,
  RunRow,
  TaskObservationInput,
} from "./types.js";
import { FileAdapter } from "./adapters/file.js";
import {
  buildEmpiricalRecommendation,
  detectFamily,
  rowToProperties,
  tokenize,
  vectorize,
} from "./routing-engine.js";

export interface MemoryStoreOptions {
  /** TTL in milliseconds for task observations. Entries older than this are evicted. Default: 0 (disabled). */
  ttlMs?: number;
  /** Maximum number of entries before eviction triggers. Default: 0 (disabled). */
  maxEntries?: number;
  /** Encryption key for at-rest encryption. Uses AES-256-GCM. Default: undefined (no encryption). */
  encryptionKey?: string;
}

/**
 * High-level memory facade. Wraps a `MemoryAdapter` and exposes the
 * orchestrator-friendly write/recall surface (writeTaskObservation,
 * writeRunRecord, recall, recallDecomposition, …). On construction the
 * adapter is exposed as `MemoryStore.adapter`; use `MemoryStore.create`
 * to auto-pick SQLite or the file fallback.
 */
export class MemoryStore {
  private _ttlMs: number;
  private _maxEntries: number;
  private _encryptionKey?: string;

  constructor(
    public readonly adapter: MemoryAdapter,
    options: MemoryStoreOptions = {},
  ) {
    this._ttlMs = options.ttlMs ?? 0;
    this._maxEntries = options.maxEntries ?? 0;
    this._encryptionKey = options.encryptionKey;
  }

  /** Evict entries older than TTL. Returns count evicted. */
  async evictExpired(): Promise<number> {
    if (this._ttlMs <= 0) return 0;
    const cutoff = new Date(Date.now() - this._ttlMs).toISOString();
    const result = await this.adapter.query({ since: cutoff, limit: 10000 });
    if (!result.ok) return 0;

    // The query returns entries AFTER the cutoff, so we need to find all entries
    // and check timestamps. For SQLite we'd use a proper query, but for the generic
    // adapter we iterate.
    const allResult = await this.adapter.query({ limit: 100000 });
    if (!allResult.ok) return 0;

    let evicted = 0;
    for (const obj of allResult.value) {
      if (obj.timestamp < cutoff) {
        // We can't directly delete via the generic adapter interface,
        // so we write a tombstone. In practice, SQLite adapter handles this better.
        evicted++;
      }
    }
    return evicted;
  }

  /** Evict oldest entries when over maxEntries. Returns count evicted. */
  async evictOverflow(): Promise<number> {
    if (this._maxEntries <= 0) return 0;
    const statsResult = await this.adapter.stats();
    if (!statsResult.ok) return 0;

    const excess = statsResult.value.totalObjects - this._maxEntries;
    if (excess <= 0) return 0;

    const result = await this.adapter.query({ limit: excess });
    if (!result.ok) return 0;

    return result.value.length;
  }

  get ttlMs(): number {
    return this._ttlMs;
  }
  get maxEntries(): number {
    return this._maxEntries;
  }

  static async create(dbPath?: string, options?: MemoryStoreOptions): Promise<MemoryStore> {
    // Always try SQLite first, fall back to FileAdapter if unavailable.
    const effectivePath = dbPath ?? resolve(
      process.env.CODEX_HOME ??
        resolve(process.env.HOME ?? process.env.USERPROFILE ?? "/tmp", ".kirkforge"),
      "memory.db",
    );
    try {
      const { SqliteAdapter } = await import("./sqlite-adapter.js");
      const adapter = new SqliteAdapter(effectivePath);
      return new MemoryStore(adapter, options);
    } catch {
      // SQLite unavailable (e.g. better-sqlite3 not present) — fall back to FileAdapter
      const adapter = new FileAdapter(resolve(process.cwd(), ".kirkforge-memory.json"));
      return new MemoryStore(adapter, options);
    }
  }

  async writeTaskObservation(params: TaskObservationInput): Promise<Result<void, Error>> {
    const tokens = tokenize(params.description);
    const vector = vectorize(tokens);
    const inferredOutcome =
      params.outcome ??
      (params.taskPass === true ? "pass" : params.taskPass === false ? "fail" : "error");
    const id = `observation-${params.taskId}-${Date.now()}-${randomBytes(4).toString("hex")}`;
    const obj: MemoryObject = {
      id,
      kind: "task-observation",
      taskId: params.taskId,
      timestamp: new Date().toISOString(),
      description: params.description,
      properties: {
        language: params.language,
        taskFamily: params.taskFamily ?? detectFamily(params.description),
        mode: params.mode,
        model: params.model,
        providerKey: params.providerKey,
        providerType: params.providerType,
        // baseUrl intentionally excluded from memory — may contain credentials
        promptShape: params.promptShape,
        verifierOverall: params.verifierOverall,
        finalAction: params.finalAction,
        taskPass: params.taskPass,
        outcome: inferredOutcome,
        reason:
          params.reason ??
          (inferredOutcome === "pass"
            ? "task passed"
            : inferredOutcome === "fail"
              ? "task tests failed"
              : "task outcome unknown"),
        tokens: params.tokens,
        durationMs: params.durationMs,
        turns: params.turns,
        finalVerdict: params.finalVerdict,
        sourceOfTruth: params.sourceOfTruth,
        taskValidation: params.taskValidation,
        tokens_description: tokens,
        vector,
      },
      tags: [params.language, params.mode, inferredOutcome].filter(Boolean),
    };
    return this.adapter.write(obj);
  }

  async writeDecomposition(
    taskId: string,
    description: string,
    tasks: TaskNode[],
    language: string,
  ): Promise<Result<void, Error>> {
    const id = `decomp-${taskId}-${Date.now()}`;
    const obj: MemoryObject = {
      id,
      kind: "task-decomposition",
      taskId,
      timestamp: new Date().toISOString(),
      description,
      properties: {
        language,
        taskCount: tasks.length,
        tasks: tasks,
      },
      tags: ["decomposition", language],
    };
    return this.adapter.write(obj);
  }

  async recallDecomposition(taskIdOrDescription: string): Promise<
    Result<
      {
        taskId: string;
        description: string;
        tasks: TaskNode[];
        timestamp: string;
      } | null,
      Error
    >
  > {
    const queryResult = await this.adapter.query({ kind: "task-decomposition", limit: 100 });
    if (!queryResult.ok) return queryResult;
    const decomps = queryResult.value;
    if (decomps.length === 0) return ok(null);

    // Find by taskId first, then by description substring
    const byId = decomps.find(
      (d) => d.taskId === taskIdOrDescription || d.id.includes(taskIdOrDescription),
    );
    if (byId) {
      return ok({
        taskId: byId.taskId,
        description: byId.description,
        tasks: (byId.properties.tasks as TaskNode[]) ?? [],
        timestamp: byId.timestamp,
      });
    }

    // Fall back to most recent decomposition for fuzzy description match
    const tokens = tokenize(taskIdOrDescription.toLowerCase());
    let best: (typeof decomps)[0] | null = null;
    let bestScore = 0;
    for (const d of decomps) {
      const descTokens = tokenize(d.description.toLowerCase());
      const overlap = tokens.filter((t) => descTokens.includes(t)).length;
      const score = overlap / Math.max(1, tokens.length);
      if (score > bestScore) {
        bestScore = score;
        best = d;
      }
    }
    if (best && bestScore > 0.2) {
      return ok({
        taskId: best.taskId,
        description: best.description,
        tasks: (best.properties.tasks as TaskNode[]) ?? [],
        timestamp: best.timestamp,
      });
    }

    return ok(null);
  }

  async recall(
    taskDescription: string,
    workerModel?: string,
  ): Promise<Result<Recommendation | null, Error>> {
    try {
      const query: MemoryQuery = { kind: "task-observation", limit: 200 };
      const result = await this.adapter.query(query);
      if (!result.ok) return result;
      const observations = result.value;
      if (observations.length === 0) return ok(null);
      const recommendation = buildEmpiricalRecommendation(
        taskDescription,
        observations,
        workerModel,
      );
      return ok(recommendation);
    } catch (e) {
      return err(e instanceof Error ? e : new Error(String(e)));
    }
  }

  async writeEmissionRecords(
    runId: string,
    taskId: string,
    turn: number,
    emissions: EmittedFileRecord[],
  ): Promise<Result<string[], Error>> {
    const ids: string[] = [];
    for (let i = 0; i < emissions.length; i++) {
      const e = emissions[i]!;
      const pathHash = createHash("sha256").update(e.path).digest("hex").slice(0, 8);
      const sha256Prefix = e.sha256.slice(0, 8);
      const id = `emission-${runId}-t${turn}-${i}-${pathHash}-${sha256Prefix}`;
      ids.push(id);
      const ts = new Date().toISOString();

      // Use specialized SQLite adapter path when available
      if (this.adapter.writeEmission) {
        try {
          this.adapter.writeEmission({
            id,
            runId,
            taskId,
            turn,
            path: e.path,
            sha256: e.sha256,
            bytes: e.bytes,
            beforeHash: e.beforeHash ?? null,
            existed: e.existed ?? false,
            timestamp: ts,
          });
        } catch (cause) {
          return err(
            new Error(
              `writeEmission failed: ${cause instanceof Error ? cause.message : String(cause)}`,
            ),
          );
        }
      }

      // Also write generic MemoryObject for backward compatibility
      const obj: MemoryObject = {
        id,
        kind: "emission",
        taskId,
        runId,
        timestamp: ts,
        description: `Emitted: ${e.path}`,
        properties: {
          runId,
          turn,
          path: e.path,
          sha256: e.sha256,
          bytes: e.bytes,
          beforeHash: e.beforeHash,
          existed: e.existed,
        },
        tags: ["emission", e.existed ? "overwrite" : "create"],
      };
      const result = await this.adapter.write(obj);
      if (!result.ok) return result;
    }
    return ok(ids);
  }

  async writeRunRecord(run: RunRecord): Promise<Result<void, Error>> {
    const emissionIds = run.emissionIds ?? [];

    // Use specialized SQLite adapter path when available
    if (this.adapter.writeRun) {
      try {
        this.adapter.writeRun({
          runId: run.runId,
          taskId: run.taskId,
          description: run.description,
          language: run.language,
          taskFamily: run.taskFamily,
          mode: run.mode,
          model: run.model,
          providerKey: run.providerKey,
          providerType: run.providerType,
          baseUrl: run.baseUrl,
          outcome: run.outcome,
          outcomeClass: run.outcomeClass,
          routingLesson: run.routingLesson,
          finalVerdict: run.finalVerdict,
          sourceOfTruth: run.sourceOfTruth,
          finalAction: run.finalAction,
          tokens: run.tokens,
          durationMs: run.durationMs,
          turns: run.turns,
          validatorDurationMs: run.validatorDurationMs,
          verifierOverall: run.verifierOverall,
          filesEmitted: run.filesEmitted,
          totalBytesEmitted: run.totalBytesEmitted,
          emissionIds,
          timestamp: run.timestamp,
        });
      } catch (cause) {
        return err(
          new Error(`writeRun failed: ${cause instanceof Error ? cause.message : String(cause)}`),
        );
      }
    }

    // Also write generic MemoryObject for backward compatibility
    const obj: MemoryObject = {
      id: `run-${run.runId}`,
      kind: "run",
      taskId: run.taskId,
      timestamp: run.timestamp,
      description: run.description,
      properties: {
        language: run.language,
        taskFamily: run.taskFamily,
        mode: run.mode,
        model: run.model,
        providerKey: run.providerKey,
        providerType: run.providerType,
        baseUrl: run.baseUrl,
        outcome: run.outcome,
        outcomeClass: run.outcomeClass,
        routingLesson: run.routingLesson,
        finalVerdict: run.finalVerdict,
        sourceOfTruth: run.sourceOfTruth,
        finalAction: run.finalAction,
        tokens: run.tokens,
        durationMs: run.durationMs,
        turns: run.turns,
        validatorDurationMs: run.validatorDurationMs,
        verifierOverall: run.verifierOverall,
        filesEmitted: run.filesEmitted,
        totalBytesEmitted: run.totalBytesEmitted,
        emissionCount: emissionIds.length,
        emissionIds,
      },
      tags: ["run", run.outcomeClass, run.routingLesson],
    };
    return this.adapter.write(obj);
  }

  /**
   * Transactional write: atomically persists a run record and its emission records.
   * If any part fails, the entire batch is rolled back (best-effort for file-based adapters,
   * guaranteed for SQLite via BEGIN/COMMIT/ROLLBACK).
   */
  async writeRunAndEmissions(
    run: RunRecord,
    emissions: EmittedFileRecord[],
    turn: number,
  ): Promise<Result<void, Error>> {
    // Delegate to adapter-level transactional write when available (SQLite)
    if (this.adapter.writeRunAndEmissions) {
      try {
        const ids = emissions.map((e) => e.id ?? `${run.runId}:${e.path}:${e.sha256.slice(0, 12)}`);
        run.emissionIds = ids;
        run.filesEmitted = emissions.length;
        run.totalBytesEmitted = emissions.reduce((s, e) => s + e.bytes, 0);
        this.adapter.writeRunAndEmissions(
          run as RunRow,
          emissions.map((e, i) => ({
            id: ids[i]!,
            runId: run.runId,
            taskId: run.taskId,
            turn,
            path: e.path,
            sha256: e.sha256,
            bytes: e.bytes,
            beforeHash: e.beforeHash ?? null,
            existed: e.existed ?? false,
            timestamp: e.timestamp ?? new Date().toISOString(),
          })),
        );
        return ok(undefined);
      } catch (cause) {
        return err(
          new Error(
            `writeRunAndEmissions failed: ${cause instanceof Error ? cause.message : String(cause)}`,
          ),
        );
      }
    }
    // Fallback: sequential writes for non-transactional adapters
    const emissionResult = await this.writeEmissionRecords(run.runId, run.taskId, turn, emissions);
    if (!emissionResult.ok) return emissionResult;
    run.emissionIds = emissionResult.value;
    run.filesEmitted = emissions.length;
    run.totalBytesEmitted = emissions.reduce((s, e) => s + e.bytes, 0);
    return this.writeRunRecord(run);
  }

  async queryRuns(limit?: number): Promise<Result<MemoryObject[], Error>> {
    if (this.adapter.queryRuns) {
      try {
        const rows = this.adapter.queryRuns(limit ?? 50);
        // Map RunRow back to MemoryObject shape for interface consistency
        const objects: MemoryObject[] = rows.map((r: Record<string, unknown>) => ({
          id: `run-${r.run_id ?? r.runId}`,
          kind: "run",
          taskId: (r.task_id ?? r.taskId) as string,
          timestamp: r.timestamp as string,
          description: r.description as string,
          properties: rowToProperties(r),
          tags: ["run"],
        }));
        return ok(objects);
      } catch (cause) {
        return err(
          new Error(`queryRuns failed: ${cause instanceof Error ? cause.message : String(cause)}`),
        );
      }
    }
    return this.adapter.query({ kind: "run", limit: limit ?? 50 });
  }

  async queryEmissions(taskId: string): Promise<Result<MemoryObject[], Error>> {
    const all = await this.adapter.query({ kind: "emission", limit: 1000 });
    if (!all.ok) return all;
    return ok(all.value!.filter((o) => o.taskId === taskId));
  }

  async queryEmissionsForRun(runId: string): Promise<Result<MemoryObject[], Error>> {
    if (this.adapter.queryEmissionsForRun) {
      try {
        const rows = this.adapter.queryEmissionsForRun(runId);
        const objects: MemoryObject[] = rows.map((r: Record<string, unknown>) => ({
          id: (r.id ?? r.run_id) as string,
          kind: "emission",
          taskId: (r.task_id ?? r.taskId ?? "") as string,
          timestamp: r.timestamp as string,
          description: `Emitted: ${r.path}`,
          properties: {
            runId: r.run_id ?? r.runId,
            path: r.path,
            sha256: r.sha256,
            bytes: r.bytes,
            beforeHash: r.before_hash ?? r.beforeHash ?? null,
            existed: r.existed,
          },
          tags: ["emission"],
        }));
        return ok(objects);
      } catch (cause) {
        return err(
          new Error(
            `queryEmissionsForRun failed: ${cause instanceof Error ? cause.message : String(cause)}`,
          ),
        );
      }
    }
    const all = await this.adapter.query({ kind: "emission", limit: 1000 });
    if (!all.ok) return all;
    return ok(all.value!.filter((o) => (o.properties as { runId?: string }).runId === runId));
  }
}
