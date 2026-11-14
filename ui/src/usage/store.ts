// The meter's UI store: holds the host's snapshot, applies the `plan_usage` event, and runs
// the cadence. A read is asked for on start, about every minute while the window is visible, on
// becoming visible again when the numbers are over a minute old, and on Refresh. The host enforces
// the 60 s floor; asking early just returns the current snapshot.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { usageOps, type UsageSnapshot } from '../api/usage.ts';

/** A little over the host's 60 s floor, so each tick is allowed to start a read. */
export const POLL_MS = 62_000;
/** Coming back to a visible window refreshes at once when the newest number is older than this. */
export const STALE_ON_SHOW_MS = 60_000;

type Listener = () => void;

export interface UsageStoreDeps {
  api: Api;
  bus: EventBus;
  /** Whether the window is visible now (document.visibilityState). */
  visible?: () => boolean;
  /** Calls `f` whenever the window's visibility changes; returns the way to stop listening. */
  onVisibility?: (f: () => void) => () => void;
  now?: () => number;
  setInterval?: (f: () => void, ms: number) => unknown;
  clearInterval?: (h: unknown) => void;
}

export class UsageStore {
  private snap: UsageSnapshot | null = null;
  private failure: string | null = null;
  private readonly listeners = new Set<Listener>();
  private readonly ops: ReturnType<typeof usageOps>;
  private readonly deps: UsageStoreDeps;
  private off: (() => void) | undefined;
  private offVisibility: (() => void) | undefined;
  private timer: unknown;
  private started = false;

  constructor(deps: UsageStoreDeps) {
    this.deps = deps;
    this.ops = usageOps(deps.api);
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): UsageSnapshot | null {
    return this.snap;
  }

  /** Why the last ask of the host failed (the host itself being unreachable), if it did. */
  get error(): string | null {
    return this.failure;
  }

  private set(s: UsageSnapshot): void {
    this.snap = s;
    this.failure = null;
    for (const l of [...this.listeners]) l();
  }

  /** Begin: read the stored snapshot, ask for a read, listen for the event, start the timer. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.off = this.deps.bus.on('plan_usage', (e) => {
      const { kind: _kind, ...rest } = e;
      this.set(rest as unknown as UsageSnapshot);
    });
    void this.ops.get().then((s) => (this.snap ? undefined : this.set(s)), (e) => this.fail(e));
    void this.refresh();
    const every = this.deps.setInterval ?? ((f, ms) => setInterval(f, ms));
    this.timer = every(() => {
      if (this.deps.visible?.() ?? true) void this.refresh();
    }, POLL_MS);
    this.offVisibility = this.deps.onVisibility?.(() => {
      if (!(this.deps.visible?.() ?? true)) return;
      const at = this.snap?.reading?.fetchedAt;
      if (at === undefined || (this.deps.now ?? Date.now)() - at > STALE_ON_SHOW_MS) void this.refresh();
    });
  }

  stop(): void {
    this.off?.();
    this.offVisibility?.();
    this.offVisibility = undefined;
    if (this.timer !== undefined) (this.deps.clearInterval ?? ((h) => clearInterval(h as ReturnType<typeof setInterval>)))(this.timer);
    this.timer = undefined;
    this.started = false;
  }

  private fail(e: unknown): void {
    this.failure = String((e as Error)?.message ?? e);
    for (const l of [...this.listeners]) l();
  }

  async refresh(): Promise<void> {
    try {
      this.set(await this.ops.refresh());
    } catch (e) {
      this.fail(e);
    }
  }
}

const stores = new WeakMap<Api, UsageStore>();

/** The one store per app (keyed by its api). */
export function usageStoreFor(api: Api, bus: EventBus): UsageStore {
  let s = stores.get(api);
  if (!s) {
    const doc = typeof document === 'undefined' ? undefined : document;
    s = new UsageStore({
      api,
      bus,
      visible: () => !doc || doc.visibilityState !== 'hidden',
      onVisibility: (f) => {
        doc?.addEventListener('visibilitychange', f);
        return () => doc?.removeEventListener('visibilitychange', f);
      },
    });
    stores.set(api, s);
  }
  return s;
}
