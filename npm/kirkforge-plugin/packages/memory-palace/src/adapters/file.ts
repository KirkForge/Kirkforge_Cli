import { ok, err, type Result } from "@kirkforge/core-types";
import { readFile, writeFile, mkdir, rename, copyFile } from "node:fs/promises";
import { openSync, writeFileSync, fsyncSync, closeSync, rmSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { randomBytes } from "node:crypto";
import type { MemoryAdapter, MemoryObject, MemoryQuery, MemoryStats } from "../types.js";

/**
 * JSON-file-backed memory adapter. Concurrency is handled by a `.lock`
 * file with an `EEXIST` retry loop; the data file is written to a temp
 * path and atomically renamed so partial writes never leave the file
 * in a corrupt state. If the file is found in a corrupt state on load
 * it is copied to `<file>.corrupt` and an empty in-memory cache is
 * returned (subsequent operations return an error from `loadError`).
 */
export class FileAdapter implements MemoryAdapter {
  private objects: MemoryObject[] = [];
  private filePath: string;
  private lockPath: string;
  private dirty = false;
  private loaded = false;
  private loadError: Error | null = null;
  private loading: Promise<void> | null = null;

  constructor(filePath: string) {
    this.filePath = resolve(filePath);
    this.lockPath = this.filePath + ".lock";
  }

  private async acquireLock(timeoutMs = 5000): Promise<number | null> {
    const started = Date.now();
    while (true) {
      try {
        const fd = openSync(this.lockPath, "wx");
        writeFileSync(fd, String(process.pid), "utf-8");
        return fd;
      } catch (e) {
        const err = e as NodeJS.ErrnoException;
        if (err.code !== "EEXIST" && err.code !== "ENOENT") return null;
      }
      if (Date.now() - started > timeoutMs) return null;
      await new Promise((r) => setTimeout(r, 50));
    }
  }

  private releaseLock(fd: number): void {
    try {
      fsyncSync(fd);
      closeSync(fd);
      rmSync(this.lockPath);
    } catch {
      /* best-effort */
    }
  }

  private async load(): Promise<void> {
    if (this.loaded) return;
    if (this.loading) {
      await this.loading;
      return;
    }
    this.loading = (async () => {
      try {
        const raw = await readFile(this.filePath, "utf-8");
        const parsed = JSON.parse(raw);
        if (!Array.isArray(parsed)) {
          throw new Error(`Memory file does not contain an array: ${this.filePath}`);
        }
        const malformed = parsed.findIndex(
          (obj: unknown) =>
            typeof obj !== "object" ||
            obj === null ||
            typeof (obj as Record<string, unknown>).id !== "string" ||
            typeof (obj as Record<string, unknown>).kind !== "string" ||
            typeof (obj as Record<string, unknown>).taskId !== "string" ||
            typeof (obj as Record<string, unknown>).timestamp !== "string",
        );
        if (malformed !== -1) {
          this.loadError = new Error(
            `Memory file contains malformed object at index ${malformed}: each object must have string id, kind, taskId, and timestamp. File: ${this.filePath}`,
          );
          this.objects = [];
          this.loaded = true;
          return;
        }
        this.objects = parsed as MemoryObject[];
        this.loaded = true;
      } catch (cause) {
        const errObj = cause as NodeJS.ErrnoException;
        if (errObj.code === "ENOENT") {
          this.objects = [];
          this.loaded = true;
          return;
        }
        const corruptPath = this.filePath + ".corrupt";
        try {
          await copyFile(this.filePath, corruptPath);
        } catch {
          /* best effort */
        }
        this.loadError = new Error(
          `Memory file corrupted: ${this.filePath}. Backup saved to ${corruptPath}. Original error: ${errObj.message}`,
        );
        this.objects = [];
        this.loaded = true;
      } finally {
        this.loading = null;
      }
    })();
    await this.loading;
  }

  private async flush(): Promise<void> {
    if (!this.dirty) return;
    const lockFd = await this.acquireLock(5000);
    if (lockFd === null) throw new Error("FileAdapter: could not acquire lock for flush after 5s");
    try {
      await mkdir(dirname(this.filePath), { recursive: true });
      const tmpPath = this.filePath + ".tmp." + Date.now() + "." + randomBytes(4).toString("hex");
      const data = JSON.stringify(this.objects);
      try {
        await writeFile(tmpPath, data, "utf-8");
        try {
          const fd = openSync(tmpPath, "r");
          try {
            fsyncSync(fd);
          } finally {
            closeSync(fd);
          }
        } catch {
          /* fsync best effort */
        }
        await rename(tmpPath, this.filePath);
        this.dirty = false;
      } catch (writeErr) {
        this.dirty = true;
        try {
          const { unlink } = await import("node:fs/promises");
          await unlink(tmpPath).catch(() => {});
        } catch {
          /* cleanup best effort */
        }
        throw writeErr;
      }
    } finally {
      this.releaseLock(lockFd);
    }
  }

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    const lockFd = await this.acquireLock(3000);
    if (lockFd === null)
      return err(new Error("FileAdapter: could not acquire lock for write after 3s"));
    try {
      await this.load();
      if (this.loadError) {
        this.releaseLock(lockFd);
        return err(this.loadError);
      }
      this.objects.push(obj);
      this.dirty = true;
      return ok(undefined);
    } finally {
      this.releaseLock(lockFd);
    }
  }

  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    await this.load();
    if (this.loadError) return err(this.loadError);
    return ok(this.objects.find((o) => o.id === id) ?? null);
  }

  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    await this.load();
    if (this.loadError) return err(this.loadError);
    let results = [...this.objects];
    if (q.kind) results = results.filter((o) => o.kind === q.kind);
    if (q.tags) results = results.filter((o) => q.tags!.some((t) => o.tags.includes(t)));
    if (q.since) results = results.filter((o) => o.timestamp >= q.since!);
    results.sort((a, b) => b.timestamp.localeCompare(a.timestamp));
    if (q.limit) results = results.slice(0, q.limit);
    return ok(results);
  }

  async stats(): Promise<Result<MemoryStats, Error>> {
    await this.load();
    if (this.loadError) return err(this.loadError);
    return ok({
      totalObjects: this.objects.length,
      lastWrite: this.objects[this.objects.length - 1]?.timestamp ?? "never",
    });
  }

  async persist(): Promise<void> {
    try {
      await this.flush();
    } catch (e) {
      process.stderr.write(
        `[memory-palace] persist failed: ${e instanceof Error ? e.message : String(e)}\n`,
      );
      this.dirty = true;
    }
  }

  clear(): void {
    this.objects = [];
    this.dirty = true;
  }
}
