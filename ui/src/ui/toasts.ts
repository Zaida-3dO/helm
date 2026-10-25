// The generic toast slot. One toast at a time, chosen by priority;
// a lower kind is hidden (not queued) while a higher one is up and shows only if it is still live
// when the higher one goes. Each lifetime restarts when its content changes. The settings-change
// toast gets its own slot; the completion stack is separate (bottom-right).

export type ToastKind = 'error' | 'info' | 'restored' | 'undo' | 'reorganize';

const PRIORITY: Record<ToastKind, number> = { error: 1, info: 1, restored: 2, undo: 3, reorganize: 4 };
const LIFETIME_MS: Record<ToastKind, number> = { error: 5000, info: 5000, restored: 5000, undo: 3000, reorganize: 5000 };

export interface ToastSpec {
  kind: ToastKind;
  /** The glyph: the notice kinds draw an alert or an info mark by default. */
  icon?: 'archive';
  text: string;
  action?: { label: string; hint?: string; run: () => void };
  /** Override the kind's lifetime (tests, or a notice that must linger). */
  ttlMs?: number;
}

export interface LiveToast extends ToastSpec {
  id: number;
  expiresAt: number;
}

type Listener = () => void;

export class ToastSlot {
  private live: LiveToast[] = [];
  private seq = 0;
  private readonly listeners = new Set<Listener>();
  private snapshot: LiveToast | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private readonly now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  /** The toast on screen, or null. */
  current = (): LiveToast | null => this.snapshot;

  private recompute(): void {
    const t = this.now();
    this.live = this.live.filter((x) => x.expiresAt > t);
    const top = [...this.live].sort((a, b) => PRIORITY[a.kind] - PRIORITY[b.kind] || b.id - a.id)[0] ?? null;
    if (top !== this.snapshot) {
      this.snapshot = top;
      for (const l of [...this.listeners]) l();
    }
    clearTimeout(this.timer);
    const next = Math.min(...this.live.map((x) => x.expiresAt));
    if (Number.isFinite(next)) this.timer = setTimeout(() => this.recompute(), Math.max(0, next - t) + 5);
  }

  /** Show a toast. A newer toast of the same kind replaces the older. Returns its id. */
  show(spec: ToastSpec): number {
    const id = ++this.seq;
    const ttl = spec.ttlMs ?? LIFETIME_MS[spec.kind];
    this.live = this.live.filter((x) => x.kind !== spec.kind && !(PRIORITY[x.kind] === 1 && PRIORITY[spec.kind] === 1));
    this.live.push({ ...spec, id, expiresAt: this.now() + ttl });
    this.recompute();
    return id;
  }

  dismiss(id: number): void {
    this.live = this.live.filter((x) => x.id !== id);
    this.recompute();
  }

  /** Re-check expiry (the timer does this; tests call it with a fake clock). */
  tick(): void {
    this.recompute();
  }
}

/** The app's main toast slot. `toast.show({kind: 'error', text})` from anywhere. */
export const toast = new ToastSlot();
