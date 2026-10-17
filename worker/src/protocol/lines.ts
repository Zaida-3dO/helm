// Newline-delimited JSON framing, shared by every reader and writer in the worker.

/** 96 MiB per line, in either direction. */
export const MAX_RECORD_BYTES = 96 * 1024 * 1024;

export type LineFault =
  /** A line grew past the limit before its newline arrived. */
  | { kind: "too_long"; bytes: number; limit: number }
  /** The stream ended in the middle of a line (no trailing newline). */
  | { kind: "truncated"; bytes: number }
  /** A complete line that is not a JSON object. */
  | { kind: "bad_json"; detail: string; preview: string };

export function describeFault(f: LineFault): string {
  switch (f.kind) {
    case "too_long":
      return `record too long: more than ${f.limit} bytes without a newline`;
    case "truncated":
      return `record truncated: stream ended after ${f.bytes} bytes of an unfinished line`;
    case "bad_json":
      return `record is not a JSON object (${f.detail}): ${f.preview}`;
  }
}

export interface LineReaderHandlers {
  onRecord: (record: Record<string, unknown>) => void;
  onFault: (fault: LineFault) => void;
  onEnd?: () => void;
}

/**
 * Splits a byte stream into JSON records. After the first fault the reader stops delivering
 * records: a bad record fails the run, so nothing after it is trusted.
 */
export class LineReader {
  private chunks: Buffer[] = [];
  private pending = 0;
  private failed = false;
  private ended = false;
  private readonly limit: number;
  private readonly h: LineReaderHandlers;

  constructor(handlers: LineReaderHandlers, limit: number = MAX_RECORD_BYTES) {
    this.h = handlers;
    this.limit = limit;
  }

  get hasFailed(): boolean {
    return this.failed;
  }

  push(chunk: Buffer | string): void {
    if (this.failed || this.ended) return;
    let buf = typeof chunk === "string" ? Buffer.from(chunk, "utf8") : chunk;
    while (buf.length > 0 && !this.failed) {
      const nl = buf.indexOf(0x0a);
      if (nl === -1) {
        this.pending += buf.length;
        if (this.pending > this.limit) {
          this.fail({ kind: "too_long", bytes: this.pending, limit: this.limit });
          return;
        }
        this.chunks.push(buf);
        return;
      }
      const head = buf.subarray(0, nl);
      if (this.pending + head.length > this.limit) {
        this.fail({ kind: "too_long", bytes: this.pending + head.length, limit: this.limit });
        return;
      }
      this.chunks.push(head);
      const line = Buffer.concat(this.chunks);
      this.chunks = [];
      this.pending = 0;
      buf = buf.subarray(nl + 1);
      this.deliver(line);
    }
  }

  end(): void {
    if (this.ended) return;
    this.ended = true;
    if (!this.failed && this.pending > 0) {
      this.fail({ kind: "truncated", bytes: this.pending });
      return;
    }
    this.h.onEnd?.();
  }

  private deliver(raw: Buffer): void {
    let text = raw.toString("utf8");
    if (text.endsWith("\r")) text = text.slice(0, -1);
    if (text.trim() === "") return; // blank lines are ignored, never a fault
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch (e) {
      this.fail({ kind: "bad_json", detail: (e as Error).message, preview: text.slice(0, 120) });
      return;
    }
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      this.fail({ kind: "bad_json", detail: "not an object", preview: text.slice(0, 120) });
      return;
    }
    this.h.onRecord(value as Record<string, unknown>);
  }

  private fail(f: LineFault): void {
    this.failed = true;
    this.chunks = [];
    this.pending = 0;
    this.h.onFault(f);
  }
}

/** Serialises one record as a line, refusing anything over the limit. */
export function encodeRecord(record: unknown, limit: number = MAX_RECORD_BYTES): string {
  const line = JSON.stringify(record);
  if (line === undefined) throw new Error("record is not serialisable");
  if (Buffer.byteLength(line, "utf8") + 1 > limit) {
    throw new RangeError(`outgoing record exceeds ${limit} bytes`);
  }
  return line + "\n";
}
