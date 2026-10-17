// The worker side of the host-tool request channel.
//
// A call creates a private folder for its id, writes one `chat_request` line, then polls for the
// host's `response.json` every 50 ms. On an answer, or on the caller's cancel or timeout, it
// removes the folder and writes `chat_cancel` (which also aborts a host task still running).
// Refusals made here, before the line is written, always mean nothing changed.

import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import type { Json } from "../protocol/types.ts";

export const POLL_MS = 50;
export const MAX_CONCURRENT = 8;
export const DEFAULT_TIMEOUT_MS = 20_000;
/** The worker waits this much longer than the host's own timeout, so the host's answer wins. */
export const GRACE_MS = 2_000;

export type Changed = "no" | "maybe";
export type CallResult = { ok: true; result: unknown } | { ok: false; error: string; changed: Changed };

export interface CallOptions {
  limitBytes: number;
  timeoutMs: number;
  /** A mutating tool: a lost reply may still have changed something. */
  mutating: boolean;
  caller?: { child: boolean; toolCallId?: string };
  signal?: AbortSignal;
}

export const NOTHING_CHANGED = " Nothing changed.";
export const MAY_HAVE_HAPPENED = " The change may have happened: check the current state before retrying, and never retry automatically.";

export interface ChannelOptions {
  /** The chat's tools directory (launch.json `chatToolsDirectory`). */
  dir: string;
  emit: (rec: Json) => void;
  pollMs?: number;
  maxConcurrent?: number;
  /** Extra wait past the tool's own timeout. */
  graceMs?: number;
}

export class RequestChannel {
  private readonly o: Required<ChannelOptions>;
  private inflight = 0;

  constructor(o: ChannelOptions) {
    this.o = { pollMs: POLL_MS, maxConcurrent: MAX_CONCURRENT, graceMs: GRACE_MS, ...o };
  }

  get running(): number {
    return this.inflight;
  }

  async call(operation: string, args: unknown, opts: CallOptions): Promise<CallResult> {
    const body = args ?? {};
    const size = Buffer.byteLength(JSON.stringify(body));
    if (size > opts.limitBytes) {
      return { ok: false, error: `The request is ${size} bytes; ${operation} takes at most ${Math.floor(opts.limitBytes / 1024)} KiB.${NOTHING_CHANGED}`, changed: "no" };
    }
    if (this.inflight >= this.o.maxConcurrent) {
      return { ok: false, error: `This chat already has ${this.o.maxConcurrent} host-tool calls running.${NOTHING_CHANGED}`, changed: "no" };
    }
    if (opts.signal?.aborted) return { ok: false, error: `The call was cancelled.${NOTHING_CHANGED}`, changed: "no" };
    const id = randomUUID();
    const folder = path.join(this.o.dir, id);
    try {
      fs.mkdirSync(folder, { recursive: true, mode: 0o700 });
    } catch (e) {
      return { ok: false, error: `The request folder could not be made (${(e as Error).message}).${NOTHING_CHANGED}`, changed: "no" };
    }
    this.inflight += 1;
    const rec: Json = { type: "chat_request", id, operation, args: body };
    if (opts.caller) rec.caller = opts.caller;
    try {
      this.o.emit(rec);
      return await this.wait(id, folder, operation, opts);
    } finally {
      this.inflight -= 1;
      fs.rmSync(folder, { recursive: true, force: true });
      this.o.emit({ type: "chat_cancel", id });
    }
  }

  private wait(id: string, folder: string, operation: string, opts: CallOptions): Promise<CallResult> {
    const file = path.join(folder, "response.json");
    const lost = (why: string): CallResult =>
      opts.mutating
        ? { ok: false, error: `${why}${MAY_HAVE_HAPPENED}`, changed: "maybe" }
        : { ok: false, error: `${why}${NOTHING_CHANGED}`, changed: "no" };
    return new Promise((resolve) => {
      let done = false;
      const finish = (r: CallResult) => {
        if (done) return;
        done = true;
        clearInterval(poll);
        clearTimeout(timer);
        opts.signal?.removeEventListener("abort", onAbort);
        resolve(r);
      };
      const onAbort = () => finish(lost(`${operation} was cancelled after it was sent.`));
      const timer = setTimeout(() => finish(lost(`${operation} got no answer within ${Math.round(opts.timeoutMs / 1000)} s.`)), opts.timeoutMs + this.o.graceMs);
      const poll = setInterval(() => {
        let text: string;
        try {
          text = fs.readFileSync(file, "utf8");
        } catch {
          return; // not written yet
        }
        try {
          const v = JSON.parse(text) as Json;
          if (v.id !== id) return finish(lost(`${operation} got an answer for another request.`));
          if (v.ok === true) return finish({ ok: true, result: v.result });
          const changed: Changed = v.changed === "maybe" ? "maybe" : "no";
          finish({ ok: false, error: typeof v.error === "string" ? v.error : `${operation} failed.`, changed });
        } catch {
          // A torn read: the host writes atomically, so try again on the next tick.
        }
      }, this.o.pollMs);
      opts.signal?.addEventListener("abort", onAbort, { once: true });
    });
  }
}
