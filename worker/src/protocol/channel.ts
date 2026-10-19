// The worker's stdout: events and responses to the host. Enforces exactly one `response` per
// id-bearing command and the outgoing record limit.

import type { Writable } from "node:stream";
import { encodeRecord, MAX_RECORD_BYTES } from "./lines.ts";
import type { Json } from "./types.ts";

export interface ResponseBody {
  success: boolean;
  data?: unknown;
  error?: string;
  uncertain?: boolean;
  /** Send-timeline marks (timing.ts): epoch milliseconds, on `get_state` and prompt answers. */
  timing?: Record<string, number>;
}

export type Diagnostic = (msg: string) => void;

export class HostChannel {
  private readonly out: Writable;
  private readonly limit: number;
  private readonly diag: Diagnostic;
  private readonly answered = new Set<string>();
  private closed = false;

  constructor(out: Writable, diag: Diagnostic, limit: number = MAX_RECORD_BYTES) {
    this.out = out;
    this.diag = diag;
    this.limit = limit;
  }

  emit(event: Json): void {
    if (this.closed) return;
    let line: string;
    try {
      line = encodeRecord(event, this.limit);
    } catch (e) {
      // Never put an oversize record on the wire: the host would fail the run on it anyway, so
      // say why instead.
      this.diag(`dropping outgoing ${String(event.type)}: ${(e as Error).message}`);
      line = encodeRecord({
        type: "worker_error",
        error: `worker tried to send an oversize ${String(event.type)} record`,
      });
    }
    this.out.write(line);
  }

  /** Returns false (and writes nothing) if this id was already answered. */
  respond(id: string, command: string, body: ResponseBody): boolean {
    if (this.answered.has(id)) {
      this.diag(`second response for ${command} ${id} suppressed`);
      return false;
    }
    this.answered.add(id);
    const rec: Json = { type: "response", id, command, success: body.success };
    if (body.data !== undefined) rec.data = body.data;
    if (body.error !== undefined) rec.error = body.error;
    if (body.uncertain) rec.uncertain = true;
    if (body.timing) rec.timing = body.timing;
    this.emit(rec);
    return true;
  }

  hasAnswered(id: string): boolean {
    return this.answered.has(id);
  }

  close(): void {
    this.closed = true;
  }

  /** Resolves once everything written so far has been handed to the OS. */
  flush(): Promise<void> {
    return new Promise((resolve) => {
      if (!this.out.writableNeedDrain) return resolve();
      this.out.once("drain", () => resolve());
    });
  }
}
