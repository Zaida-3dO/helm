// The Tasks view's way to read the board: at most four reads in flight, a first-in-first-out
// queue behind them, one deadline per read that includes the time spent queued, and a cancel sent
// to the host when a read times out or is aborted. A read that settles is never revived: a late
// answer is dropped. Plain TypeScript, tested under node:test.

import { ApiError, type Api } from '../api/client.ts';
import { boardOps, type ReadParams, type ReadResult } from '../api/board.ts';

export const MAX_ACTIVE = 4;
export const MAX_QUEUED = 256;
export const DEFAULT_DEADLINE_MS = 15_000;
export const MIN_DEADLINE_MS = 100;

export type BoardErrorCode =
  | 'busy'
  | 'cancelled'
  | 'timed_out'
  | 'not_started'
  | 'not_configured'
  | 'token_rejected'
  | 'unreachable'
  | 'board_error'
  | 'bad_args';

export class BoardError extends Error {
  readonly code: BoardErrorCode;
  constructor(code: BoardErrorCode, message: string) {
    super(message);
    this.name = 'BoardError';
    this.code = code;
  }
}

const KNOWN: readonly BoardErrorCode[] = ['busy', 'cancelled', 'timed_out', 'not_started', 'not_configured', 'token_rejected', 'unreachable', 'board_error', 'bad_args'];

/** An op failure as a `BoardError`, with a plain sentence. */
export function toBoardError(e: unknown): BoardError {
  if (e instanceof BoardError) return e;
  if (e instanceof ApiError) {
    const code = (KNOWN as readonly string[]).includes(e.code) ? (e.code as BoardErrorCode) : 'board_error';
    return new BoardError(code, e.message);
  }
  return new BoardError('board_error', e instanceof Error ? e.message : String(e));
}

export interface ReadOptions {
  /** Read only over a connection that is already up; never reach for the saved credentials. */
  attachOnly?: boolean;
  /** Overall deadline from the moment of the call; the default is 15 s, never under 100 ms. */
  timeoutMs?: number;
  signal?: AbortSignal;
}

interface Job {
  id: string;
  params: ReadParams;
  attachOnly: boolean;
  deadlineAt: number;
  settle: (r: { ok: true; value: unknown } | { ok: false; error: BoardError }) => void;
  settled: boolean;
  sent: boolean;
}

/** What a reader sends over: the local `board_read`/`board_cancel`, or the same reads carried
 * to the PC that runs a mirrored chat (`devices_board_read`/`devices_board_cancel`). */
export interface BoardOpsLike {
  read(params: ReadParams, requestId: string, opts: { attachOnly: boolean; timeoutMs: number }): Promise<{ action: string; data: unknown }>;
  cancel(requestId: string): Promise<unknown>;
}

/** Reads of the task board on the PC that runs a mirrored chat, sent as its own board ops;
 * the board's address and token never leave that PC. */
export function remoteBoardOps(api: Api, chatId: string): BoardOpsLike {
  return {
    read: (params, requestId, opts) =>
      api<{ action: string; data: unknown }>('devices_board_read', { chatId, ...params, requestId, ...opts }),
    cancel: (requestId) => api<Record<string, never>>('devices_board_cancel', { chatId, requestId }),
  };
}

export class BoardReader {
  private readonly ops: BoardOpsLike;
  private active = 0;
  private readonly queue: Job[] = [];
  private seq = 0;
  private readonly maxActive: number;
  private readonly maxQueued: number;
  private readonly newId: () => string;

  constructor(source: Api | BoardOpsLike, opts: { maxActive?: number; maxQueued?: number; newId?: () => string } = {}) {
    // An `Api` is a function; the remote shape is an object of the same two calls.
    this.ops = typeof source === 'function' ? boardOps(source) : source;
    this.maxActive = opts.maxActive ?? MAX_ACTIVE;
    this.maxQueued = opts.maxQueued ?? MAX_QUEUED;
    this.newId = opts.newId ?? (() => `tr-${Date.now().toString(36)}-${(this.seq++).toString(36)}`);
  }

  /** Reads in flight now (for tests and the status line). */
  get inFlight(): number {
    return this.active;
  }

  get queued(): number {
    return this.queue.length;
  }

  read<A extends ReadParams['action']>(params: ReadParams & { action: A }, opts: ReadOptions = {}): Promise<ReadResult<A>> {
    if (opts.signal?.aborted) return Promise.reject(new BoardError('cancelled', 'The read was cancelled.'));
    if (this.queue.length >= this.maxQueued && this.active >= this.maxActive) {
      return Promise.reject(new BoardError('busy', 'The board is being asked for too much at once; try again in a moment.'));
    }
    const deadline = Math.max(MIN_DEADLINE_MS, Math.min(opts.timeoutMs ?? DEFAULT_DEADLINE_MS, DEFAULT_DEADLINE_MS));
    return new Promise<ReadResult<A>>((resolve, reject) => {
      const job: Job = {
        id: this.newId(),
        params,
        attachOnly: opts.attachOnly ?? false,
        deadlineAt: Date.now() + deadline,
        settled: false,
        sent: false,
        settle: (r) => {
          if (job.settled) return;
          job.settled = true;
          clearTimeout(timer);
          opts.signal?.removeEventListener('abort', onAbort);
          if (r.ok) resolve(r.value as ReadResult<A>);
          else reject(r.error);
        },
      };
      const finish = (error: BoardError) => {
        if (job.settled) return;
        const wasQueued = this.queue.indexOf(job);
        if (wasQueued >= 0) this.queue.splice(wasQueued, 1);
        // Tell the host to stop waiting; the slot is released when the call returns.
        if (job.sent) void this.ops.cancel(job.id).catch(() => undefined);
        job.settle({ ok: false, error });
      };
      const timer = setTimeout(() => finish(new BoardError('timed_out', "The task board didn't answer in time.")), deadline);
      const onAbort = () => finish(new BoardError('cancelled', 'The read was cancelled.'));
      opts.signal?.addEventListener('abort', onAbort, { once: true });
      this.queue.push(job);
      this.pump();
    });
  }

  private pump(): void {
    while (this.active < this.maxActive) {
      const job = this.queue.shift();
      if (!job) return;
      if (job.settled) continue;
      this.active++;
      job.sent = true;
      // The host's own clock gets what is left of this read's deadline.
      const timeoutMs = Math.max(MIN_DEADLINE_MS, job.deadlineAt - Date.now());
      void this.ops
        .read(job.params, job.id, { attachOnly: job.attachOnly, timeoutMs })
        .then(
          (r) => job.settle({ ok: true, value: r.data }),
          (e: unknown) => job.settle({ ok: false, error: toBoardError(e) }),
        )
        .finally(() => {
          this.active--;
          this.pump();
        });
    }
  }
}
