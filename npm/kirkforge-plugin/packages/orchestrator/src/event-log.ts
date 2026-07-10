import type { EventBus } from "@kirkforge/core-events";
import type { KirkForgeEvent } from "@kirkforge/core-types";
import { mkdirSync, openSync, appendFileSync, closeSync, readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { createHmac } from "node:crypto";
import type { Logger } from "@kirkforge/core-logging";

/**
 * Append-only event log writer with hash chaining for tamper detection.
 *
 * Each entry is a JSONL line with:
 *   - `_logged`: ISO timestamp
 *   - `_prev`: SHA-256 HMAC of the previous entry (null for first)
 *   - `_seq`: monotonically increasing sequence number
 *
 * The chain can be verified by replaying entries: each `_prev` must match
 * HMAC(previous raw line). A break indicates tampering or log truncation.
 */
export class EventLogger {
  private fd: number | null = null;
  private path: string;
  private prevHash: string | null = null;
  private seq = 0;
  private hmacKey: Buffer;

  constructor(
    private eventBus: EventBus,
    hmacSecret: string,
    logPath?: string,
    private logger?: Logger,
  ) {
    const secret = hmacSecret || process.env.EVENT_LOG_HMAC_SECRET;
    if (!secret) {
      throw new Error("EventLogger requires EVENT_LOG_HMAC_SECRET env var or explicit hmacSecret");
    }
    this.path = logPath ?? resolve(process.cwd(), ".kirkforge/event-log.jsonl");
    this.hmacKey = Buffer.from(secret, "utf-8");

    // Recover prevHash and sequence by reading last line of existing log
    this._recoverChain();
    this._wire();
  }

  /** Read the last line of an existing log to recover chain state. */
  private _recoverChain(): void {
    if (!existsSync(this.path)) return;
    try {
      const content = readFileSync(this.path, "utf-8").trimEnd();
      if (!content) return;
      const lines = content.split("\n");
      const lastLine = lines[lines.length - 1];
      if (!lastLine) return;
      const lastEntry = JSON.parse(lastLine);
      if (typeof lastEntry._seq === "number") {
        this.seq = lastEntry._seq + 1;
      } else {
        this.seq = lines.length;
      }
      // Store the computed hash of this last line as prevHash
      this.prevHash = this._hmac(lastLine);
      this.logger?.debug(
        `[event-log] Recovered chain: seq=${this.seq}, prevHash=${this.prevHash?.slice(0, 12)}...`,
      );
    } catch {
      // If recovery fails, start fresh
      this.seq = 0;
      this.prevHash = null;
    }
  }

  private _wire(): void {
    const eventKinds: KirkForgeEvent["kind"][] = [
      "verify.lint",
      "verify.types",
      "verify.security",
      "state.changes",
      "state.graph",
      "artifact.blocked",
      "artifact.unterminated",
      "artifact.truncated",
      "artifact.emitted",
    ];

    for (const kind of eventKinds) {
      this.eventBus.on(kind as Parameters<typeof this.eventBus.on>[0], async (event) => {
        try {
          this._append(event);
        } catch (e) {
          this.logger?.warn(
            `[event-log] Failed to persist: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
        return { ok: true, value: undefined };
      });
    }
  }

  /** Open the log file lazily on first write. */
  private _ensureOpen(): boolean {
    if (this.fd !== null) return true;
    try {
      mkdirSync(dirname(this.path), { recursive: true });
      this.fd = openSync(this.path, "a");
      return true;
    } catch {
      return false;
    }
  }

  private _hmac(line: string): string {
    return createHmac("sha256", this.hmacKey).update(line, "utf-8").digest("hex");
  }

  /**
   * Verify the integrity of the entire log by replaying the hash chain.
   * Returns { valid: boolean, brokenAt: number | null }.
   */
  static verifyLog(
    path: string,
    hmacSecret?: string,
  ): { valid: boolean; brokenAt: number | null; entries: number } {
    const secret = hmacSecret ?? process.env.EVENT_LOG_HMAC_SECRET;
    if (!secret) {
      return { valid: false, brokenAt: null, entries: 0 };
    }
    const key = Buffer.from(secret, "utf-8");

    try {
      const content = readFileSync(path, "utf-8").trimEnd();
      if (!content) return { valid: true, brokenAt: null, entries: 0 };

      const lines = content.split("\n");
      let prevHash: string | null = null;

      for (let i = 0; i < lines.length; i++) {
        const line = lines[i]!;
        try {
          const entry = JSON.parse(line);
          if (entry._prev !== prevHash) {
            return { valid: false, brokenAt: i, entries: lines.length };
          }
          prevHash = createHmac("sha256", key).update(line, "utf-8").digest("hex");
        } catch {
          return { valid: false, brokenAt: i, entries: lines.length };
        }
      }

      return { valid: true, brokenAt: null, entries: lines.length };
    } catch {
      return { valid: false, brokenAt: null, entries: 0 };
    }
  }

  private _append(event: KirkForgeEvent): void {
    if (!this._ensureOpen()) return;

    const currentSeq = this.seq;
    const entry = {
      ...event,
      _logged: new Date().toISOString(),
      _prev: this.prevHash,
      _seq: currentSeq,
    };

    const line = JSON.stringify(entry) + "\n";
    try {
      appendFileSync(this.fd!, line, "utf-8");
      // Update prevHash from the line we just wrote
      this.prevHash = this._hmac(line.slice(0, -1)); // hash without trailing newline
      this.seq = currentSeq + 1;
    } catch {
      try {
        closeSync(this.fd!);
      } catch {
        /* ok */
      }
      this.fd = null;
    }
  }

  close(): void {
    if (this.fd !== null) {
      try {
        closeSync(this.fd);
      } catch {
        /* ok */
      }
      this.fd = null;
    }
  }

  /** Current chain length. */
  get sequenceNumber(): number {
    return this.seq;
  }
}
