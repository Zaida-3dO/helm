// The run-slot queue as the renderer sees it. Only a few starts wait for a free slot (another
// chat's message, an agent's first message to a chat it creates, a scheduled prompt); the
// host publishes the whole queue on every change as a `resource-queue` event. This keeps one
// copy of it, shared by the waiting bar, the sidebar's row mark and Settings.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';

export interface SlotWait {
  chatId: string;
  submissionId: string;
  /** 1-based, in the order the waits will start. */
  position: number;
  since: number;
  head: boolean;
}

export interface SlotSettings {
  enabled: boolean;
  lowRunningChats: number;
}

export interface SlotQueueView {
  settings: SlotSettings;
  limit: number;
  running: string[];
  waiting: SlotWait[];
  /** Why automated starts are held (plan usage near its limit), or null. */
  held: string | null;
}

export const MIN_SLOTS = 2;
export const MAX_SLOTS = 20;

export function slotQueueOps(api: Api) {
  return {
    snapshot: () => api<SlotQueueView>('resource_queue_snapshot', {}),
    startNow: (chatId: string, submissionId: string) => api<SlotQueueView>('resource_queue_start_now', { chatId, submissionId }),
    cancel: (submissionId: string) => api<SlotQueueView>('resource_queue_cancel', { submissionId }),
    saveSettings: (settings: Partial<SlotSettings>) => api<SlotQueueView>('run_slots_settings', { settings }),
  };
}

/** "next in line", "2nd in line", "3rd in line"... */
export function linePosition(n: number): string {
  if (n <= 1) return 'next in line';
  const tens = n % 100;
  const suffix = tens >= 11 && tens <= 13 ? 'th' : ({ 1: 'st', 2: 'nd', 3: 'rd' } as Record<number, string>)[n % 10] ?? 'th';
  return `${n}${suffix} in line`;
}

/** Why the waits are waiting: the usage hold, full slots, or an earlier wait ahead of them. */
export function waitReason(v: SlotQueueView): string {
  if (v.held) return v.held.replace(/\.$/, '');
  if (v.running.length >= v.limit) return `All ${v.limit} run slots are busy`;
  return 'Behind an earlier message';
}

/** Each waiting submission's note for its bar: the reason, then its place in line. */
export function waitNotes(v: SlotQueueView | null): ReadonlyMap<string, string> {
  const notes = new Map<string, string>();
  if (!v) return notes;
  const reason = waitReason(v);
  for (const w of v.waiting) notes.set(w.submissionId, `${reason} · ${linePosition(w.position)}`);
  return notes;
}

/** The `resource-queue` event carries the view's fields beside its kind. */
function viewOf(raw: Record<string, unknown>): SlotQueueView | null {
  if (!Array.isArray(raw.waiting) || !Array.isArray(raw.running)) return null;
  const { kind: _kind, ...rest } = raw;
  return { held: null, ...rest } as unknown as SlotQueueView;
}

type Listener = () => void;

export class SlotQueueStore {
  private view: SlotQueueView | null = null;
  private readonly listeners = new Set<Listener>();
  private started = false;
  private waitingChats = new Set<string>();

  private readonly api: Api;
  private readonly bus: EventBus;

  constructor(api: Api, bus: EventBus) {
    this.api = api;
    this.bus = bus;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): SlotQueueView | null {
    return this.view;
  }

  isWaiting = (chatId: string): boolean => this.waitingChats.has(chatId);

  private set(v: SlotQueueView): void {
    this.view = v;
    this.waitingChats = new Set(v.waiting.map((w) => w.chatId));
    for (const l of [...this.listeners]) l();
  }

  /** Read the queue once and follow its event. Safe to call again. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.bus.on('resource-queue', (e) => {
      const v = viewOf(e);
      if (v) this.set(v);
    });
    slotQueueOps(this.api)
      .snapshot()
      // An event that arrived while the read was out is newer than the read.
      .then(
        (v) => {
          if (!this.view) this.set(v);
        },
        () => undefined,
      );
  }

  /** Adopt a view an op answered with. */
  adopt(v: SlotQueueView): void {
    this.set(v);
  }
}

const stores = new WeakMap<Api, SlotQueueStore>();

export function slotQueueStoreFor(api: Api, bus: EventBus): SlotQueueStore {
  let s = stores.get(api);
  if (!s) {
    s = new SlotQueueStore(api, bus);
    stores.set(api, s);
  }
  return s;
}
