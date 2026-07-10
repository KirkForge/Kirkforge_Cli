// kirkforge-lint-disable no-sql-inject
import { ok, err, type Result } from "@kirkforge/core-types";
import type { MemoryAdapter, MemoryObject, MemoryQuery, MemoryStats } from "./index.js";
import { resolve, dirname } from "node:path";
import { mkdirSync, existsSync, copyFileSync, readdirSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";

/** Metadata returned by SqliteAdapter.backup(). */
export interface BackupMetadata {
  /** Absolute path of the backup file. */
  filePath: string;
  /** Size of the backup file in bytes. */
  sizeBytes: number;
  /** SHA-256 hash of the backup file contents. */
  sha256: string;
  /** Schema version at time of backup. */
  schemaVersion: number | null;
  /** ISO timestamp when the backup was created. */
  timestamp: string;
  /** Number of rows in each table at backup time. */
  rowCount: { observations: number; runs: number; emissions: number };
}

/** Current schema version. Increment when adding migrations. */
export const SCHEMA_VERSION = 3;

export class SqliteAdapter implements MemoryAdapter {
  private db: any;
  private filePath: string;
  // Cached prepared statements (B9)
  private _stmtInsertObs!: any;
  private _stmtInsertRun!: any;
  private _stmtInsertEmission!: any;
  private _stmtDeleteEmissionsByRun!: any;
  private _stmtBeginTx!: any;
  private _stmtCommit!: any;
  private _stmtRollback!: any;

  constructor(filePath: string) {
    let Database: any;
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      Database = require("better-sqlite3");
    } catch {
      throw new Error(
        "SQLite adapter requires optional dependency better-sqlite3. " +
          "Install it (npm install better-sqlite3) or use FileMemoryAdapter instead.",
      );
    }
    this.filePath = resolve(filePath);
    mkdirSync(dirname(this.filePath), { recursive: true });
    this.db = new Database(this.filePath);
    this._initSchema();
    this._prepareStatements();
  }

  private _initSchema(): void {
    this.db.exec("PRAGMA journal_mode=WAL");
    this.db.exec("PRAGMA busy_timeout=5000");
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS observations (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        task_id TEXT NOT NULL,
        timestamp TEXT NOT NULL,
        description TEXT NOT NULL,
        properties TEXT NOT NULL,
        tags TEXT NOT NULL
      )
    `);
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_obs_kind ON observations(kind)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_obs_task_id ON observations(task_id)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_obs_timestamp ON observations(timestamp)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_obs_tags ON observations(tags)");

    this.db.exec(`
      CREATE TABLE IF NOT EXISTS runs (
        run_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL,
        description TEXT NOT NULL,
        language TEXT NOT NULL,
        task_family TEXT,
        mode TEXT NOT NULL,
        model TEXT NOT NULL,
        provider_key TEXT NOT NULL DEFAULT '',
        provider_type TEXT NOT NULL DEFAULT '',
        base_url TEXT,
        outcome TEXT NOT NULL,
        outcome_class TEXT NOT NULL,
        routing_lesson TEXT NOT NULL DEFAULT 'neutral',
        final_verdict TEXT NOT NULL,
        source_of_truth TEXT NOT NULL,
        final_action TEXT NOT NULL,
        tokens INTEGER NOT NULL DEFAULT 0,
        duration_ms INTEGER NOT NULL DEFAULT 0,
        turns INTEGER NOT NULL DEFAULT 0,
        validator_duration_ms INTEGER NOT NULL DEFAULT 0,
        verifier_overall TEXT,
        files_emitted INTEGER NOT NULL DEFAULT 0,
        total_bytes_emitted INTEGER NOT NULL DEFAULT 0,
        emission_ids TEXT NOT NULL DEFAULT '[]',
        timestamp TEXT NOT NULL
      )
    `);
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_runs_task_id ON runs(task_id)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_runs_model ON runs(model)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_runs_outcome_class ON runs(outcome_class)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_runs_timestamp ON runs(timestamp)");
    // Migration: add emission_ids column if missing (pre-1.1 databases)
    try {
      this.db.exec("ALTER TABLE runs ADD COLUMN emission_ids TEXT NOT NULL DEFAULT '[]'");
    } catch {
      /* column already exists */
    }

    this.db.exec(`
      CREATE TABLE IF NOT EXISTS emissions (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        task_id TEXT NOT NULL,
        turn INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        bytes INTEGER NOT NULL DEFAULT 0,
        before_hash TEXT,
        existed INTEGER NOT NULL DEFAULT 0,
        timestamp TEXT NOT NULL,
        FOREIGN KEY (run_id) REFERENCES runs(run_id)
      )
    `);
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_emissions_task_id ON emissions(task_id)");
    this.db.exec("CREATE INDEX IF NOT EXISTS idx_emissions_run_id ON emissions(run_id)");
    // Migration: add turn column if missing (pre-1.1 databases)
    try {
      this.db.exec("ALTER TABLE emissions ADD COLUMN turn INTEGER NOT NULL DEFAULT 0");
    } catch {
      /* column already exists */
    }

    // Schema versioning for future migrations
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS schema_migrations (
        version INTEGER PRIMARY KEY,
        applied_at TEXT NOT NULL
      )
    `);
    // Seed initial version if empty, then run pending migrations
    const versionRow = this.db.prepare("SELECT MAX(version) as v FROM schema_migrations").get() as
      | { v: number | null }
      | undefined;
    const currentVersion = versionRow?.v ?? 0;
    if (currentVersion === 0) {
      this.db
        .prepare("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)")
        .run(1, new Date().toISOString());
    }

    // Run pending migrations
    this._runMigrations(currentVersion === 0 ? 1 : currentVersion);
  }

  /**
   * Run any pending schema migrations. Each migration is a numbered step
   * that upgrades the schema from version N to N+1.
   *
   * Migrations are tracked in the schema_migrations table. A migration is
   * only applied if its version number is greater than the current version.
   *
   * To add a migration:
   *   1. Add it to the MIGRATIONS array below with the next version number.
   *   2. Each migration is a function that takes the Database instance.
   *   3. Migrations MUST be idempotent (safe to re-run).
   *   4. Migrations are applied in order, within a transaction.
   */
  private static readonly MIGRATIONS: Array<{
    version: number;
    description: string;
    up: (db: any) => void;
  }> = [
    // Migration 2: Add run_outcome_reason column to runs table
    {
      version: 2,
      description: "Add outcome_reason column to runs",
      up(db: any): void {
        // Check if column already exists (idempotent)
        const columns = db.prepare("PRAGMA table_info(runs)").all() as Array<{ name: string }>;
        const hasOutcomeReason = columns.some((c) => c.name === "outcome_reason");
        if (!hasOutcomeReason) {
          db.exec("ALTER TABLE runs ADD COLUMN outcome_reason TEXT");
        }
      },
    },
    // Migration 3: Add routing_bias column to observations table
    {
      version: 3,
      description: "Add routing_bias column to observations",
      up(db: any): void {
        const columns = db.prepare("PRAGMA table_info(observations)").all() as Array<{
          name: string;
        }>;
        const hasRoutingBias = columns.some((c) => c.name === "routing_bias");
        if (!hasRoutingBias) {
          db.exec("ALTER TABLE observations ADD COLUMN routing_bias TEXT");
        }
      },
    },
    // ── Add future migrations here ──────────────────────────────────────
    // {
    //   version: 4,
    //   description: "Description of migration",
    //   up(db: any): void {
    //     db.exec("...");
    //   },
    // },
  ];

  private _runMigrations(fromVersion: number): void {
    const pending = SqliteAdapter.MIGRATIONS.filter((m) => m.version > fromVersion);
    if (pending.length === 0) return;

    const runMigration = this.db.transaction(() => {
      for (const migration of pending) {
        migration.up(this.db);
        this.db
          .prepare("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)")
          .run(migration.version, new Date().toISOString());
      }
    });

    try {
      runMigration();
    } catch (cause) {
      throw new Error(
        `Schema migration failed at version ${fromVersion}: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
  }

  /** Re-prepare cached statements. Called after reopen (e.g. restore). */
  private _prepareStatements(): void {
    this._stmtInsertObs = this.db.prepare(
      "INSERT OR REPLACE INTO observations (id, kind, task_id, timestamp, description, properties, tags) VALUES (?, ?, ?, ?, ?, ?, ?)",
    );
    this._stmtInsertRun = this.db.prepare(
      `INSERT OR REPLACE INTO runs
       (run_id, task_id, description, language, task_family, mode, model,
        provider_key, provider_type, base_url, outcome, outcome_class,
        routing_lesson, final_verdict, source_of_truth, final_action,
        tokens, duration_ms, turns, validator_duration_ms, verifier_overall,
        files_emitted, total_bytes_emitted, emission_ids, timestamp)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    this._stmtInsertEmission = this.db.prepare(
      `INSERT OR REPLACE INTO emissions
       (id, run_id, task_id, turn, path, sha256, bytes, before_hash, existed, timestamp)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    this._stmtDeleteEmissionsByRun = this.db.prepare("DELETE FROM emissions WHERE run_id = ?");
    this._stmtBeginTx = this.db.prepare("BEGIN IMMEDIATE");
    this._stmtCommit = this.db.prepare("COMMIT");
    this._stmtRollback = this.db.prepare("ROLLBACK");
  }

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    try {
      this._stmtInsertObs.run(
        obj.id,
        obj.kind,
        obj.taskId,
        obj.timestamp,
        obj.description,
        JSON.stringify(obj.properties),
        JSON.stringify(obj.tags),
      );
      return ok(undefined);
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter write failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    try {
      const row = this.db.prepare("SELECT * FROM observations WHERE id = ?").get(id) as
        | Row
        | undefined;
      if (!row) return ok(null);
      return ok(this._rowToObject(row));
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter read failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    try {
      const conditions: string[] = [];
      const params: unknown[] = [];

      if (q.kind) {
        conditions.push("kind = ?");
        params.push(q.kind);
      }
      if (q.since) {
        conditions.push("timestamp >= ?");
        params.push(q.since);
      }

      // For tag queries, we do a LIKE match on the JSON array stored in tags column.
      // This is a simplified approach — for large datasets, a junction table would be better.
      if (q.tags && q.tags.length > 0) {
        for (const tag of q.tags) {
          conditions.push("tags LIKE ?");
          params.push(`%"${tag}"%`);
        }
      }

      const where = conditions.length > 0 ? `WHERE ${conditions.join(" AND ")}` : "";
      const limit = q.limit ?? 1000;
      const sql = `SELECT * FROM observations ${where} ORDER BY timestamp DESC LIMIT ?`;
      params.push(limit);

      const rows = this.db.prepare(sql).all(...params) as Row[];
      return ok(rows.map((r) => this._rowToObject(r)));
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter query failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  async stats(): Promise<Result<MemoryStats, Error>> {
    try {
      const countRow = this.db.prepare("SELECT COUNT(*) as cnt FROM observations").get() as {
        cnt: number;
      };
      const lastRow = this.db
        .prepare("SELECT timestamp FROM observations ORDER BY timestamp DESC LIMIT 1")
        .get() as { timestamp: string } | undefined;
      return ok({
        totalObjects: countRow.cnt,
        lastWrite: lastRow?.timestamp ?? "never",
      });
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter stats failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  writeRun(run: {
    runId: string;
    taskId: string;
    description: string;
    language: string;
    taskFamily?: string;
    mode: string;
    model: string;
    providerKey: string;
    providerType: string;
    baseUrl?: string;
    outcome: string;
    outcomeClass: string;
    routingLesson: string;
    finalVerdict: string;
    sourceOfTruth: string;
    finalAction: string;
    tokens: number;
    durationMs: number;
    turns: number;
    validatorDurationMs: number;
    verifierOverall?: string;
    filesEmitted: number;
    totalBytesEmitted: number;
    emissionIds: string[];
    timestamp: string;
  }): void {
    this._stmtInsertRun.run(
      run.runId,
      run.taskId,
      run.description,
      run.language,
      run.taskFamily ?? null,
      run.mode,
      run.model,
      run.providerKey,
      run.providerType,
      run.baseUrl ?? null,
      run.outcome,
      run.outcomeClass,
      run.routingLesson,
      run.finalVerdict,
      run.sourceOfTruth,
      run.finalAction,
      run.tokens,
      run.durationMs,
      run.turns,
      run.validatorDurationMs,
      run.verifierOverall ?? null,
      run.filesEmitted,
      run.totalBytesEmitted,
      JSON.stringify(run.emissionIds ?? []),
      run.timestamp,
    );
  }

  writeEmission(emission: {
    id: string;
    runId: string;
    taskId: string;
    turn: number;
    path: string;
    sha256: string;
    bytes: number;
    beforeHash: string | null;
    existed: boolean;
    timestamp: string;
  }): void {
    this._stmtInsertEmission.run(
      emission.id,
      emission.runId,
      emission.taskId,
      emission.turn,
      emission.path,
      emission.sha256,
      emission.bytes,
      emission.beforeHash,
      emission.existed ? 1 : 0,
      emission.timestamp,
    );
  }

  queryRuns(limit = 50): Array<Record<string, unknown>> {
    const stmt = this.db.prepare("SELECT * FROM runs ORDER BY timestamp DESC LIMIT ?");
    return stmt.all(limit) as Array<Record<string, unknown>>;
  }

  writeRunAndEmissions(
    run: {
      runId: string;
      taskId: string;
      description: string;
      language: string;
      taskFamily?: string;
      mode: string;
      model: string;
      providerKey: string;
      providerType: string;
      baseUrl?: string;
      outcome: string;
      outcomeClass: string;
      routingLesson: string;
      finalVerdict: string;
      sourceOfTruth: string;
      finalAction: string;
      tokens: number;
      durationMs: number;
      turns: number;
      validatorDurationMs: number;
      verifierOverall?: string;
      filesEmitted: number;
      totalBytesEmitted: number;
      emissionIds: string[];
      timestamp: string;
    },
    emissions: Array<{
      id: string;
      runId: string;
      taskId: string;
      turn: number;
      path: string;
      sha256: string;
      bytes: number;
      beforeHash: string | null;
      existed: boolean;
      timestamp: string;
    }>,
  ): void {
    this._stmtBeginTx.run();
    try {
      this.writeRun(run);
      // Remove stale emissions from a prior write of the same run
      this._stmtDeleteEmissionsByRun.run(run.runId);
      for (const emission of emissions) {
        this.writeEmission(emission);
      }
      this._stmtCommit.run();
    } catch (e) {
      try {
        this._stmtRollback.run();
      } catch {
        /* best-effort */
      }
      throw e;
    }
  }

  queryEmissionsForRun(runId: string): Array<Record<string, unknown>> {
    const stmt = this.db.prepare("SELECT * FROM emissions WHERE run_id = ? ORDER BY path");
    return stmt.all(runId) as Array<Record<string, unknown>>;
  }

  async persist(): Promise<void> {
    // WAL checkpoint to flush to disk
    this.db.exec("PRAGMA wal_checkpoint(TRUNCATE)");
  }

  /**
   * Create a consistent backup of the SQLite database.
   * Uses better-sqlite3's native backup API which produces a snapshot
   * safe to use even while writes are in progress.
   *
   * @param destPath - Path for the backup file. If omitted, uses
   *   `<dbPath>.backup.<timestamp>`.
   * @returns BackupMetadata with file info, checksum, and row counts.
   */
  async backup(destPath?: string): Promise<Result<BackupMetadata, Error>> {
    try {
      // WAL checkpoint first for a consistent snapshot
      await this.persist();

      const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
      const backupPath = destPath ? resolve(destPath) : `${this.filePath}.backup.${timestamp}`;

      // Ensure destination directory exists
      const backupDir = dirname(backupPath);
      if (!existsSync(backupDir)) mkdirSync(backupDir, { recursive: true });

      // Use better-sqlite3's native backup API — safe concurrent snapshot
      await this.db.backup(backupPath);

      // Verify the backup by opening it read-only and counting rows
      let Database: any;
      try {
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        Database = require("better-sqlite3");
      } catch {
        return err(new Error("better-sqlite3 not available for backup verification"));
      }

      const verifyDb = new Database(backupPath, { readonly: true });
      const obsCount = (
        verifyDb.prepare("SELECT COUNT(*) as cnt FROM observations").get() as { cnt: number }
      ).cnt;
      const runsCount = (
        verifyDb.prepare("SELECT COUNT(*) as cnt FROM runs").get() as { cnt: number }
      ).cnt;
      const emitCount = (
        verifyDb.prepare("SELECT COUNT(*) as cnt FROM emissions").get() as { cnt: number }
      ).cnt;
      verifyDb.close();

      // Compute SHA-256 of the backup file
      const fileContents = readFileSync(backupPath);
      const sha256 = createHash("sha256").update(fileContents).digest("hex");

      return ok({
        filePath: backupPath,
        sizeBytes: fileContents.length,
        sha256,
        schemaVersion: this.schemaVersion(),
        timestamp: new Date().toISOString(),
        rowCount: {
          observations: obsCount,
          runs: runsCount,
          emissions: emitCount,
        },
      });
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter backup failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  /**
   * Restore the database from a backup file.
   * Closes the current database, replaces it with the backup, and reopens.
   *
   * IMPORTANT: This is a destructive operation. The current database file
   * is overwritten. Callers should create a backup first if they want to
   * preserve the current state.
   *
   * @param backupPath - Path to the backup file to restore from.
   * @returns Result with the BackupMetadata of the restored database.
   */
  async restore(backupPath: string): Promise<Result<BackupMetadata, Error>> {
    try {
      const sourcePath = resolve(backupPath);
      if (!existsSync(sourcePath)) {
        return err(new Error(`Backup file not found: ${sourcePath}`));
      }

      // Compute SHA-256 of the backup file before restoring
      const fileContents = readFileSync(sourcePath);
      const sha256 = createHash("sha256").update(fileContents).digest("hex");

      // Close current database
      this.db.close();

      // Replace current database file with backup
      copyFileSync(sourcePath, this.filePath);

      // Reopen the database (triggers schema init for any missing tables)
      let Database: any;
      try {
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        Database = require("better-sqlite3");
      } catch {
        return err(new Error("better-sqlite3 not available for restore"));
      }
      this.db = new Database(this.filePath);
      this._initSchema();
      this._prepareStatements();

      // Count rows in restored database
      const obsCount = (
        this.db.prepare("SELECT COUNT(*) as cnt FROM observations").get() as { cnt: number }
      ).cnt;
      const runsCount = (
        this.db.prepare("SELECT COUNT(*) as cnt FROM runs").get() as { cnt: number }
      ).cnt;
      const emitCount = (
        this.db.prepare("SELECT COUNT(*) as cnt FROM emissions").get() as { cnt: number }
      ).cnt;

      return ok({
        filePath: sourcePath,
        sizeBytes: fileContents.length,
        sha256,
        schemaVersion: this.schemaVersion(),
        timestamp: new Date().toISOString(),
        rowCount: {
          observations: obsCount,
          runs: runsCount,
          emissions: emitCount,
        },
      });
    } catch (cause) {
      return err(
        new Error(
          `SqliteAdapter restore failed: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  /**
   * List available backups in a directory.
   * Looks for files matching the pattern `<basename>.backup.*`.
   *
   * @param directory - Directory to search. Defaults to the database file's directory.
   * @returns Array of backup file paths sorted by name (newest last).
   */
  listBackups(directory?: string): string[] {
    const dir = directory ? resolve(directory) : dirname(this.filePath);
    if (!existsSync(dir)) return [];
    const base = this.filePath.split("/").pop()!;
    const prefix = `${base}.backup.`;
    const entries = readdirSync(dir);
    return entries
      .filter((e: string) => e.startsWith(prefix))
      .sort()
      .map((e: string) => resolve(dir, e));
  }

  schemaVersion(): number | null {
    try {
      const row = this.db.prepare("SELECT MAX(version) as v FROM schema_migrations").get() as
        | { v: number | null }
        | undefined;
      return (
        row?.v ??
        (SqliteAdapter.MIGRATIONS.length > 0
          ? SqliteAdapter.MIGRATIONS[SqliteAdapter.MIGRATIONS.length - 1]!.version
          : 1)
      );
    } catch {
      return null;
    }
  }

  close(): void {
    this.db.close();
  }

  private _rowToObject(row: Row): MemoryObject {
    return {
      id: row.id as string,
      kind: row.kind as string,
      taskId: row.task_id as string,
      timestamp: row.timestamp as string,
      description: row.description as string,
      properties: JSON.parse(row.properties as string),
      tags: JSON.parse(row.tags as string),
    };
  }
}

interface Row {
  id: string;
  kind: string;
  task_id: string;
  timestamp: string;
  description: string;
  properties: string;
  tags: string;
  [key: string]: unknown;
}
// kirkforge-lint-enable no-sql-inject
