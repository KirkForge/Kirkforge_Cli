import type { KirkForgeEvent, KirkForgeEventKind } from "@kirkforge/core-types";
import { ok, err, type Result } from "@kirkforge/core-types";
import { HandlerError, BufferOverflowError, IdempotencyError } from "@kirkforge/core-errors";
import { createHash } from "crypto";

export type EventHandler<T extends KirkForgeEvent = KirkForgeEvent> = (
  event: T,
) => Promise<Result<void, HandlerError>>;

export interface EventBusOptions {
  bufferCapacity?: number;
  idempotencyCacheSize?: number;
  idempotencyTtlMs?: number;
}

interface IdempotencyEntry {
  processedAt: string;
}

function sha256(input: string): string {
  return createHash("sha256").update(input).digest("hex");
}

function makeEventId(event: KirkForgeEvent): string {
  const payload = "value" in event ? (event as { value: unknown }).value : "";
  // Include timestamp to distinguish events with identical kind/stream/sequence
  const ts = "timestamp" in event ? event.timestamp : new Date().toISOString();
  return sha256(
    JSON.stringify({
      kind: event.kind,
      streamId: event.streamId,
      sequence: event.sequence,
      payload,
      ts,
    }),
  );
}

function now(): string {
  return new Date().toISOString();
}

export class EventBus {
  private _handlers = new Map<KirkForgeEventKind, Set<EventHandler>>();
  private _buffer: KirkForgeEvent[] = [];
  private _bufferCapacity: number;
  private _idempotency = new Map<string, IdempotencyEntry>();
  private _idempotencySize: number;
  private _idempotencyTtlMs: number;
  private _sequence = 0;
  private _running = true;
  private _shuttingDown = false;
  private _inflight = 0;
  private _drainResolve: (() => void) | null = null;

  constructor(options: EventBusOptions = {}) {
    this._bufferCapacity = options.bufferCapacity ?? 1000;
    this._idempotencySize = options.idempotencyCacheSize ?? 10000;
    this._idempotencyTtlMs = options.idempotencyTtlMs ?? 300000;
  }

  on<T extends KirkForgeEvent>(kind: T["kind"], handler: EventHandler<T>): () => void {
    const set = this._handlers.get(kind) ?? new Set();
    set.add(handler as EventHandler);
    this._handlers.set(kind, set);
    return () => set.delete(handler as EventHandler);
  }

  emit(
    event: KirkForgeEvent,
  ): Promise<Result<void, HandlerError | BufferOverflowError | IdempotencyError>> {
    if (!this._running || this._shuttingDown) {
      return Promise.resolve(err(new HandlerError("emit", new Error("EventBus is not running"))));
    }

    if (this._buffer.length >= this._bufferCapacity) {
      const overflowEvent: KirkForgeEvent = {
        kind: "event.bus.overflowed",
        schemaVersion: "v3",
        sequence: ++this._sequence,
        streamId: event.streamId,
        bufferSize: this._buffer.length,
        bufferCapacity: this._bufferCapacity,
        originalEventKind: event.kind,
        originalStreamId: event.streamId,
        timestamp: now(),
      };
      return this._process(overflowEvent);
    }

    const eventId = makeEventId(event);
    if (this._idempotency.has(eventId)) {
      return Promise.resolve(err(new IdempotencyError(eventId)));
    }

    this._buffer.push(event);
    this._idempotency.set(eventId, { processedAt: now() });
    this._trimIdempotency();
    return this._process(event);
  }

  private async _process(event: KirkForgeEvent): Promise<Result<void, HandlerError>> {
    const handlers = this._handlers.get(event.kind);
    if (!handlers || handlers.size === 0) return ok(undefined);

    this._inflight++;
    try {
      const errors: HandlerError[] = [];
      for (const handler of handlers) {
        try {
          const result = await handler(event);
          if (!result.ok) errors.push(result.error);
        } catch (rawError) {
          const err = rawError instanceof Error ? rawError : new Error(String(rawError));
          errors.push(new HandlerError(handler.name, err));
        }
      }
      return errors.length > 0 ? err(errors[0]!) : ok(undefined);
    } finally {
      this._inflight = Math.max(0, this._inflight - 1);
      if (this._shuttingDown && this._inflight === 0 && this._drainResolve) {
        this._drainResolve();
      }
      const idx = this._buffer.indexOf(event);
      if (idx !== -1) this._buffer.splice(idx, 1);
      if (this._buffer.length > 500) {
        this._buffer.splice(0, this._buffer.length - 500);
      }
    }
  }

  drainBuffer(): void {
    this._buffer = [];
  }

  private _trimIdempotency(): void {
    const cutoff = Date.now() - this._idempotencyTtlMs;
    for (const [key, entry] of this._idempotency) {
      if (new Date(entry.processedAt).getTime() < cutoff) this._idempotency.delete(key);
    }
    while (this._idempotency.size > this._idempotencySize) {
      const firstKey = this._idempotency.keys().next().value;
      if (firstKey !== undefined) this._idempotency.delete(firstKey);
    }
  }

  get running(): boolean {
    return this._running;
  }
  get inflightCount(): number {
    return this._inflight;
  }
  getBufferSize(): number {
    return this._buffer.length;
  }
  getBufferCapacity(): number {
    return this._bufferCapacity;
  }

  shutdown(): void {
    this._running = false;
    this._shuttingDown = true;
  }

  async gracefulShutdown(drainTimeoutMs?: number): Promise<void> {
    this._shuttingDown = true;
    if (this._inflight === 0) {
      this._running = false;
      return;
    }
    const timeout = drainTimeoutMs ?? 10000;
    const drainPromise = new Promise<void>((r) => {
      this._drainResolve = r;
    });
    const timeoutPromise = new Promise<void>((r) => setTimeout(r, timeout));
    await Promise.race([drainPromise, timeoutPromise]);
    this._running = false;
  }
}

// Re-export audit sink module
export {
  AuditAction,
  AuditOutcome,
  AuditEvent,
  AuditSink,
  AuditSinkConfig,
  FileAuditSink,
  HttpAuditSink,
  MemoryAuditSink,
  AuditLogger,
  createAuditSink,
  WormAuditSink,
  SyslogAuditSink,
  initialHash,
  chainHashOf,
} from "./audit.js";
