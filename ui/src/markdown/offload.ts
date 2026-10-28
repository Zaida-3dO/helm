// Offloading long renders to a Web Worker: a 2 s timeout that resets the
// worker, and a cache by text shared by every consumer.
//
// The worker is injected so the timeout, reset and cache logic is testable without a browser.
import type { RenderOptions } from './render.ts';

/** The slice of the Worker API this module uses. */
export interface WorkerLike {
  postMessage(message: unknown): void;
  terminate(): void;
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: ((event: unknown) => void) | null;
}

export interface OffloaderConfig {
  createWorker: () => WorkerLike;
  /** Per-request timeout; the worker is replaced when it fires. Default 2000. */
  timeoutMs?: number;
  /** Cache entries kept (least recently used leave first). Default 300. */
  cacheEntries?: number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

interface Pending {
  resolve: (html: string) => void;
  reject: (error: Error) => void;
  timer: unknown;
  key: string;
  /** Streaming snapshots are transient, so they are not worth a cache slot. */
  cacheable: boolean;
}

export interface RenderRequest {
  id: number;
  text: string;
  options: RenderOptions;
}

export interface RenderResponse {
  id: number;
  html: string;
}

export function cacheKey(text: string, options: RenderOptions): string {
  const flags = `${options.streaming ? 's' : '-'}${options.interactive ? 'i' : '-'}${options.nested ? 'n' : '-'}`;
  return `${flags}${options.mentionRepo ?? ''}\u0000${text}`;
}

export class RenderOffloader {
  private worker: WorkerLike | null = null;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly cache = new Map<string, string>();
  private readonly timeoutMs: number;
  private readonly cacheEntries: number;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (handle: unknown) => void;
  /** How many workers were created: a reset is visible as a second one. */
  workersCreated = 0;
  private readonly config: OffloaderConfig;

  constructor(config: OffloaderConfig) {
    this.config = config;
    this.timeoutMs = config.timeoutMs ?? 2000;
    this.cacheEntries = config.cacheEntries ?? 300;
    this.setTimer = config.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = config.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  }

  /** The cached HTML for this text and these options, if any (and marks it recently used). */
  cached(text: string, options: RenderOptions = {}): string | undefined {
    const key = cacheKey(text, options);
    const hit = this.cache.get(key);
    if (hit !== undefined) {
      this.cache.delete(key);
      this.cache.set(key, hit);
    }
    return hit;
  }

  /** Stores a result computed elsewhere (the synchronous path for short text). */
  remember(text: string, options: RenderOptions, html: string): void {
    if (options.streaming) return;
    this.store(cacheKey(text, options), html);
  }

  /** Renders in the worker. Rejects on timeout (the worker is reset) or worker error. */
  render(text: string, options: RenderOptions = {}): Promise<string> {
    const key = cacheKey(text, options);
    const hit = this.cached(text, options);
    if (hit !== undefined) return Promise.resolve(hit);

    return new Promise<string>((resolve, reject) => {
      let worker: WorkerLike;
      try {
        worker = this.ensureWorker();
      } catch (error) {
        reject(error instanceof Error ? error : new Error(String(error)));
        return;
      }
      const id = this.nextId++;
      const timer = this.setTimer(() => {
        this.reset(new Error(`markdown render timed out after ${this.timeoutMs} ms`));
      }, this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer, key, cacheable: !options.streaming });
      const request: RenderRequest = { id, text, options };
      worker.postMessage(request);
    });
  }

  /** Terminates the worker and fails everything in flight; the next render starts a fresh one. */
  reset(reason: Error): void {
    const worker = this.worker;
    this.worker = null;
    if (worker) {
      worker.onmessage = null;
      worker.onerror = null;
      worker.terminate();
    }
    const failed = [...this.pending.values()];
    this.pending.clear();
    for (const p of failed) {
      this.clearTimer(p.timer);
      p.reject(reason);
    }
  }

  dispose(): void {
    this.reset(new Error('markdown offloader disposed'));
    this.cache.clear();
  }

  private ensureWorker(): WorkerLike {
    if (this.worker) return this.worker;
    const worker = this.config.createWorker();
    this.workersCreated++;
    worker.onmessage = (event) => {
      const data = event.data as RenderResponse;
      const entry = this.pending.get(data.id);
      if (!entry) return;
      this.pending.delete(data.id);
      this.clearTimer(entry.timer);
      if (entry.cacheable) this.store(entry.key, data.html);
      entry.resolve(data.html);
    };
    worker.onerror = () => this.reset(new Error('markdown worker failed'));
    this.worker = worker;
    return worker;
  }

  private store(key: string, html: string): void {
    this.cache.delete(key);
    this.cache.set(key, html);
    while (this.cache.size > this.cacheEntries) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cache.delete(oldest);
    }
  }
}
