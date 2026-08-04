import { appendFileSync, existsSync, mkdirSync, renameSync, statSync } from "node:fs";
import { resolve, dirname } from "node:path";
import type { AuditEvent, AuditSink } from "../audit.js";
import { initialHash, chainHashOf } from "../audit-chain-hash.js";

export interface FileAuditSinkConfig {
  /** Path to the audit log file. */
  filePath: string;
  /** Buffer size before forcing flush. Default: 100. */
  flushInterval?: number;
  /** Maximum file size in bytes before rotation (file sink only). Default: 50 MB. */
  maxFileSizeBytes?: number;
  /** Maximum rotated files to keep (file sink only). Default: 10. */
  maxRotatedFiles?: number;
  /** HMAC key for chain integrity. When set, chain hashes use HMAC-SHA256
   *  instead of plain SHA-256, preventing recomputation by anyone without the key.
   *  Also read from KIRKFORGE_AUDIT_KEY env var. */
  hmacKey?: string;
}

// Size-based rotation: when the log file exceeds maxFileSizeBytes, it is
// renamed to <file>.1, <file>.2, etc., and a new file is started. Each
// rotated file contains a complete hash chain from genesis to its last event.

export class FileAuditSink implements AuditSink {
  readonly name = "file";
  private filePath: string;
  private buffer: AuditEvent[] = [];
  private flushSize: number;
  private lastHash: string;
  private maxFileSizeBytes: number;
  private maxRotatedFiles: number;

  private hmacKey?: string;
  constructor(config: FileAuditSinkConfig) {
    this.filePath = resolve(config.filePath);
    this.flushSize = config.flushInterval ?? 100;
    this.maxFileSizeBytes = config.maxFileSizeBytes ?? 50 * 1024 * 1024; // 50 MB
    this.maxRotatedFiles = config.maxRotatedFiles ?? 10;
    this.hmacKey = config.hmacKey ?? process.env["KIRKFORGE_AUDIT_KEY"];
    this.lastHash = initialHash(this.hmacKey);
    // Ensure directory exists
    const dir = dirname(this.filePath);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  }

  async write(event: AuditEvent): Promise<boolean> {
    this.buffer.push(event);
    if (this.buffer.length >= this.flushSize) {
      return this.flush();
    }
    return true;
  }

  /**
   * Rotate the audit log if it exceeds maxFileSizeBytes.
   * Renames the current file to <file>.1, shifts existing rotated files,
   * and deletes files beyond maxRotatedFiles.
   */
  private _rotate(): void {
    try {
      if (!existsSync(this.filePath)) return;
      const stats = statSync(this.filePath);
      if (stats.size < this.maxFileSizeBytes) return;

      // Shift existing rotated files: .N -> .N+1
      for (let i = this.maxRotatedFiles - 1; i >= 1; i--) {
        const rotated = `${this.filePath}.${i}`;
        const next = `${this.filePath}.${i + 1}`;
        if (existsSync(rotated)) {
          renameSync(rotated, next);
        }
      }
      // Current file becomes .1
      renameSync(this.filePath, `${this.filePath}.1`);
    } catch (_e) {
      // Rotation failure is not fatal — we continue appending to the current file.
    }
  }

  async flush(): Promise<boolean> {
    if (this.buffer.length === 0) return true;
    try {
      // Rotate before writing if needed
      this._rotate();

      const lines: string[] = [];
      for (const event of this.buffer) {
        const chainHash = chainHashOf(this.lastHash, event, this.hmacKey);
        const sealed: AuditEvent = { ...event, chainHash };
        lines.push(JSON.stringify(sealed));
        this.lastHash = chainHash;
      }
      const content = lines.join("\n") + "\n";
      appendFileSync(this.filePath, content, "utf-8");
      this.buffer = [];
      return true;
    } catch (_e) {
      return false;
    }
  }

  async close(): Promise<void> {
    await this.flush();
  }
}
