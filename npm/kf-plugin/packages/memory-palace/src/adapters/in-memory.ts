import { ok, type Result } from "@kirkforge/core-types";
import type {
  MemoryAdapter,
  MemoryObject,
  MemoryQuery,
  MemoryStats,
} from "../types.js";

export class InMemoryAdapter implements MemoryAdapter {
  private objects: MemoryObject[] = [];

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    this.objects.push(obj);
    return ok(undefined);
  }
  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    return ok(this.objects.find((o) => o.id === id) ?? null);
  }
  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    let results = [...this.objects];
    if (q.kind) results = results.filter((o) => o.kind === q.kind);
    if (q.tags) results = results.filter((o) => q.tags!.some((t) => o.tags.includes(t)));
    if (q.since) results = results.filter((o) => o.timestamp >= q.since!);
    results.sort((a, b) => b.timestamp.localeCompare(a.timestamp));
    if (q.limit) results = results.slice(0, q.limit);
    return ok(results);
  }
  async stats(): Promise<Result<MemoryStats, Error>> {
    return ok({
      totalObjects: this.objects.length,
      lastWrite: this.objects[this.objects.length - 1]?.timestamp ?? "never",
    });
  }
  async persist(): Promise<void> {
    /* no-op: in-memory only */
  }
  clear(): void {
    this.objects = [];
  }
}
