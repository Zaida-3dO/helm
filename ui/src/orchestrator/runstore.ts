// The workflow run of each chat the panel is open for, plus how many workflow Allow requests wait.
// The host's `workflow_run {chatId}` event is a signal: this store re-reads through
// `workflow_run_get`. A refusal is scoped to the call that got it. Only the host's generic
// unknown-op answer makes the store stop asking on its own (an older build), and even then the
// next `workflow_run` event asks again, since a host that sends it has the op.

import type { Api } from '../api/client.ts';
import type { AppEvent, EventBus } from '../api/events.ts';
import { consentOps } from '../api/consent.ts';
import { itemLinkOps } from '../api/itemlinks.ts';
import { isWorkflowConsent } from './progress.ts';
import { workflowOps } from './api.ts';
import { showRun, type RunView } from './run.ts';

type Listener = () => void;

export interface RunEntry {
  view: RunView;
  /** Pending Allow requests of workflow kinds for this chat. */
  allowRequests: number;
  /** The Agent Standup item this chat is linked to (main link first), if any. */
  item: { itemId: string; title: string } | null;
}

export class RunStore {
  private readonly entries = new Map<string, RunEntry>();
  private readonly asked = new Set<string>();
  private readonly listeners = new Set<Listener>();
  private readonly api: Api;
  private readonly bus: EventBus;
  private started = false;
  private unsupported = false;
  version = 0;

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

  private bump(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  entry(chatId: string): RunEntry | undefined {
    return this.entries.get(chatId);
  }

  private start(): void {
    if (this.started) return;
    this.started = true;
    const again = (e: AppEvent) => {
      if (typeof e.chatId === 'string' && this.asked.has(e.chatId)) void this.refresh(e.chatId);
    };
    this.bus.on('workflow_run', (e) => {
      if (typeof e.chatId === 'string' && this.asked.has(e.chatId)) void this.refresh(e.chatId, { retry: true });
    });
    this.bus.on('workflow_chat_mode', again);
    this.bus.on('consent', again);
    this.bus.on('item_links', () => {
      for (const id of [...this.asked]) void this.refresh(id);
    });
  }

  /** Ask for a chat's run once; later reads come from events and `refresh`. */
  ensure(chatId: string): void {
    this.start();
    if (!this.asked.has(chatId)) this.asked.add(chatId);
    void this.refresh(chatId);
  }

  /** Re-read one chat. `retry` (a `workflow_run` event) asks even after an unknown-op answer. */
  async refresh(chatId: string, opts: { retry?: boolean } = {}): Promise<void> {
    if (this.unsupported && !opts.retry) return;
    let run: RunView;
    try {
      run = showRun(await workflowOps(this.api).runGet(chatId));
    } catch (e) {
      if (isUnknownOp(e)) {
        // An older host without the op: nothing to show, and stop asking until a run event.
        this.unsupported = true;
        if (this.entries.delete(chatId)) this.bump();
      } else if ((e as { code?: unknown })?.code === 'not_found') {
        // The chat is gone.
        if (this.entries.delete(chatId)) this.bump();
      }
      // Any other refusal or failure is this call's alone: keep what is shown and ask next time.
      return;
    }
    this.unsupported = false;
    let allowRequests = 0;
    try {
      const l = await consentOps(this.api).list(chatId);
      allowRequests = l.pending.filter((r) => isWorkflowConsent(r.kind)).length;
    } catch {
      // The Allow cards report their own failures.
    }
    let item: RunEntry['item'] = null;
    try {
      const links = await itemLinkOps(this.api).forChats([chatId]);
      const l = links.find((x) => x.chatId === chatId && x.isMain) ?? links.find((x) => x.chatId === chatId);
      if (l) item = { itemId: l.itemId, title: l.itemTitle ?? l.itemId.slice(0, 8) };
    } catch {
      // Links are Helm-only data; without them the panel just shows no item.
    }
    const next: RunEntry = { view: run, allowRequests, item };
    const prev = this.entries.get(chatId);
    if (prev && JSON.stringify(prev) === JSON.stringify(next)) return;
    this.entries.set(chatId, next);
    this.bump();
  }
}

/** The host's generic answer for an op it does not serve (host/crates/ipc, `ApiError::refused`). */
export const UNKNOWN_OP_MESSAGE = 'This operation is not available here.';

/** Only the exact unknown-op answer counts; a refusal with its own words is an answer from the op. */
export function isUnknownOp(e: unknown): boolean {
  const x = e as { code?: unknown; message?: unknown } | null;
  return x?.code === 'refused' && x?.message === UNKNOWN_OP_MESSAGE;
}

const stores = new WeakMap<object, RunStore>();

/** One store per event bus (the app's). */
export function runStoreFor(api: Api, bus: EventBus): RunStore {
  let s = stores.get(bus);
  if (!s) {
    s = new RunStore(api, bus);
    stores.set(bus, s);
  }
  return s;
}
