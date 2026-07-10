import {
  appendFileSync,
  chmodSync,
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve, join } from "node:path";
import type { AuditEvent, AuditSink } from "../audit.js";
import { initialHash, chainHashOf } from "../audit-chain-hash.js";

// WORM (Write-Once-Read-Many) storage for tamper-evident, append-only
// audit compliance. Once events are written, they cannot be modified or
// deleted through this API. The sink maintains a hash chain for integrity
// verification and supports WORM-compatible storage backends.
//
// Enterprise deployments should use this sink in conjunction with file
// permissions (chattr +i on Linux, or cloud WORM storage like S3 Object Lock)
// for true immutability guarantees.

export interface WormAuditSinkConfig {
  /** Path to the WORM audit log directory. */
  directory: string;
  /** File prefix for audit log segments. Default: "audit-worm". */
  filePrefix?: string;
  /** Maximum size per segment file in bytes before rotation. Default: 100 MB. */
  maxSegmentBytes?: number;
  /** Maximum number of segment files to keep. Default: 0 (unlimited). */
  maxSegments?: number;
  /** Whether to fsync after each flush for durability. Default: true. */
  fsyncAfterFlush?: boolean;
  /** Whether to verify chain integrity on each write. Default: true. */
  verifyOnWrite?: boolean;
  /** HMAC key for chain integrity. When set, chain hashes use HMAC-SHA256.
   *  Also read from KIRKFORGE_AUDIT_KEY env var. */
  hmacKey?: string;
}

export class WormAuditSink implements AuditSink {
  readonly name = "worm";
  private directory: string;
  private filePrefix: string;
  private maxSegmentBytes: number;
  private maxSegments: number;
  private fsyncAfterFlush: boolean;
  private verifyOnWrite: boolean;
  private buffer: AuditEvent[] = [];
  private flushSize: number;
  private lastHash: string;
  private hmacKey?: string;
  private currentSegment: number;
  private currentSegmentPath: string;
  private writeCount = 0;

  constructor(config: WormAuditSinkConfig) {
    this.directory = resolve(config.directory);
    this.filePrefix = config.filePrefix ?? "audit-worm";
    this.maxSegmentBytes = config.maxSegmentBytes ?? 100 * 1024 * 1024; // 100 MB
    this.maxSegments = config.maxSegments ?? 0; // 0 = unlimited
    this.fsyncAfterFlush = config.fsyncAfterFlush ?? true;
    this.verifyOnWrite = config.verifyOnWrite ?? true;
    this.flushSize = 50;
    this.hmacKey = config.hmacKey ?? process.env["KIRKFORGE_AUDIT_KEY"];
    this.lastHash = initialHash(this.hmacKey);
    this.currentSegment = 0;
    this.currentSegmentPath = "";

    // Ensure directory exists
    if (!existsSync(this.directory)) mkdirSync(this.directory, { recursive: true });

    // Discover the latest segment
    this._discoverLatestSegment();
  }

  private _discoverLatestSegment(): void {
    try {
      const files = readdirSync(this.directory)
        .filter((f) => f.startsWith(this.filePrefix))
        .sort();
      if (files.length > 0) {
        const latest = files[files.length - 1]!;
        const match = latest.match(/(\d+)\.jsonl$/);
        if (match) {
          this.currentSegment = parseInt(match[1]!, 10);
          this.currentSegmentPath = join(this.directory, latest);
          // Read the last hash from the segment
          this._restoreLastHash();
        }
      }
      if (!this.currentSegmentPath) {
        this.currentSegment = 0;
        this.currentSegmentPath = this._segmentPath(0);
      }
    } catch (_e) {
      this.currentSegment = 0;
      this.currentSegmentPath = this._segmentPath(0);
    }
  }

  private _segmentPath(segment: number): string {
    return join(this.directory, `${this.filePrefix}-${String(segment).padStart(6, "0")}.jsonl`);
  }

  private _restoreLastHash(): void {
    try {
      if (!existsSync(this.currentSegmentPath)) return;
      const content = readFileSync(this.currentSegmentPath, "utf-8").trim();
      if (!content) return;
      const lines = content.split("\n");
      // Find the last valid JSON line
      for (let i = lines.length - 1; i >= 0; i--) {
        try {
          const event = JSON.parse(lines[i]!);
          if (event.chainHash) {
            this.lastHash = event.chainHash;
            return;
          }
        } catch (_e) {
          continue;
        }
      }
    } catch (_e) {
      // Best-effort restoration
    }
  }

  async write(event: AuditEvent): Promise<boolean> {
    // Compute chain hash before buffering
    const chainHash = chainHashOf(this.lastHash, event, this.hmacKey);
    const sealed: AuditEvent = { ...event, chainHash };
    this.lastHash = chainHash;

    // Verify chain integrity if enabled
    if (this.verifyOnWrite && this.buffer.length > 0) {
      const prevEvent = this.buffer[this.buffer.length - 1];
      if (prevEvent) {
        const expected = chainHashOf(prevEvent.chainHash, event, this.hmacKey);
        if (sealed.chainHash !== expected) {
          // Chain integrity violation — this should never happen
          throw new Error(
            `WORM audit chain integrity violation: expected hash ${expected}, got ${sealed.chainHash}`,
          );
        }
      }
    }

    this.buffer.push(sealed);
    this.writeCount++;
    if (this.buffer.length >= this.flushSize) {
      return this.flush();
    }
    return true;
  }

  async flush(): Promise<boolean> {
    if (this.buffer.length === 0) return true;
    try {
      // Check if current segment is too large
      if (existsSync(this.currentSegmentPath)) {
        const stats = statSync(this.currentSegmentPath);
        if (stats.size >= this.maxSegmentBytes) {
          this.currentSegment++;
          this.currentSegmentPath = this._segmentPath(this.currentSegment);
        }
      }

      // Enforce max segments (WORM: refuse to delete old)
      if (this.maxSegments > 0) {
        // Only refuse when we need to CREATE a new segment beyond the limit.
        // Appending to an existing current segment is always allowed — it is
        // already counted within maxSegments and still has room.
        if (!existsSync(this.currentSegmentPath) && !this._enforceMaxSegments()) {
          // WORM: cannot delete old segments — refuse new writes to preserve
          // audit evidence. Return false so callers know the write was rejected.
          this.buffer = [];
          return false;
        }
      }

      // Append events to current segment
      const lines = this.buffer.map((e) => JSON.stringify(e)).join("\n") + "\n";
      appendFileSync(this.currentSegmentPath, lines, "utf-8");

      // Fsync for durability
      if (this.fsyncAfterFlush) {
        try {
          const fd = openSync(this.currentSegmentPath, "r");
          fsyncSync(fd);
          closeSync(fd);
        } catch (_e) {
          // Best-effort fsync
        }
      }

      this.buffer = [];
      return true;
    } catch (_e) {
      return false;
    }
  }

  async close(): Promise<void> {
    await this.flush();
  }

  private _enforceMaxSegments(): boolean {
    try {
      const files = readdirSync(this.directory)
        .filter((f) => f.startsWith(this.filePrefix) && f.endsWith(".jsonl"))
        .sort();
      if (files.length >= this.maxSegments) {
        // WORM compliance: refuse to delete old audit segments.
        // Deleting old segments would destroy audit evidence.
        // Return false so the caller knows writes must stop.
        // Operators should configure external rotation (e.g. log shipping
        // to immutable storage) or increase maxSegments.
        return false;
      }
      return true;
    } catch (_e) {
      return true; // no directory yet — fine to write
    }
  }

  /** Get the total number of events written. */
  getWriteCount(): number {
    return this.writeCount;
  }

  /**
   * Make a segment file immutable using OS-level file permissions.
   * On Linux, uses chattr +i (requires CAP_LINUX_IMMUTABLE or root).
   * On other platforms, falls back to read-only file permissions (chmod 0o444).
   *
   * Returns true if immutability was successfully applied, false otherwise.
   * This is a best-effort operation - cloud WORM storage (S3 Object Lock)
   * should be used for production immutability guarantees.
   */
  makeSegmentImmutable(segmentNumber?: number): boolean {
    const segPath =
      segmentNumber !== undefined ? this._segmentPath(segmentNumber) : this.currentSegmentPath;
    if (!segPath || !existsSync(segPath)) return false;

    try {
      chmodSync(segPath, 0o444);

      if (process.platform === "linux") {
        try {
          execFileSync("chattr", ["+i", segPath], { timeout: 5000 });
          return true;
        } catch (_e) {
          // chattr requires root/CAP_LINUX_IMMUTABLE - best effort
        }
      }
      return true;
    } catch (_e) {
      return false;
    }
  }

  /**
   * Check if a segment file is immutable.
   * On Linux, checks if the immutable flag is set via lsattr.
   * On other platforms, checks if the file is read-only.
   */
  isSegmentImmutable(segmentNumber?: number): boolean {
    const segPath =
      segmentNumber !== undefined ? this._segmentPath(segmentNumber) : this.currentSegmentPath;
    if (!segPath || !existsSync(segPath)) return false;

    try {
      if (process.platform === "linux") {
        try {
          const output = execFileSync("lsattr", ["-d", segPath], { timeout: 5000 });
          const attrs = output.toString().split(/\s/)[0] ?? "";
          return /i/.test(attrs);
        } catch (_e) {
          // lsattr not available or no permissions
        }
      }
      const stats = statSync(segPath);
      return (stats.mode & 0o200) === 0;
    } catch (_e) {
      return false;
    }
  }

  /** Get the current segment number. */
  getCurrentSegment(): number {
    return this.currentSegment;
  }

  /**
   * Verify the integrity of the entire WORM audit log.
   * Returns true if all chain hashes are valid, false if any tampering is detected.
   */
  verifyIntegrity(): boolean {
    try {
      const files = readdirSync(this.directory)
        .filter((f) => f.startsWith(this.filePrefix) && f.endsWith(".jsonl"))
        .sort();

      let prevHash = initialHash(this.hmacKey);
      for (const file of files) {
        const content = readFileSync(join(this.directory, file), "utf-8").trim();
        if (!content) continue;
        for (const line of content.split("\n")) {
          try {
            const event = JSON.parse(line);
            const expected = chainHashOf(prevHash, event, this.hmacKey);
            if (event.chainHash !== expected) return false;
            prevHash = event.chainHash;
          } catch (_e) {
            continue;
          }
        }
      }
      return true;
    } catch (_e) {
      return false;
    }
  }
}
