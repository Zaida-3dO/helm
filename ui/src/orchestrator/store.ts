// The UI store for the Orchestrator: the workflow settings (master switch, default for new chats)
// and each chat's switch state. The host's `workflow_settings {}` and `workflow_chat_mode
// {chatId}` events are signals; this store re-reads through the ops. Nothing is shown
// optimistically: the switch changes when the host's answer comes back.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { workflowOps, type ChatMode, type ChatModeView, type WorkflowOps, type WorkflowSettings } from './api.ts';

type Listener = () => void;

export interface OrchestratorDeps {
  api: Api;
  bus: EventBus;
}

export class OrchestratorStore {
  private cfg: WorkflowSettings | null = null;
  private readonly views = new Map<string, ChatModeView>();
  private readonly asked = new Set<string>();
  private readonly errors = new Map<string, string>();
  private readonly listeners = new Set<Listener>();
  readonly ops: WorkflowOps;
  private started = false;
  /** Bumped on every change so a React snapshot read sees a new value. */
  version = 0;

  private readonly deps: OrchestratorDeps;

  constructor(deps: OrchestratorDeps) {
    this.deps = deps;
    this.ops = workflowOps(deps.api);
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

  get settings(): WorkflowSettings | null {
    return this.cfg;
  }

  view(chatId: string): ChatModeView | undefined {
    return this.views.get(chatId);
  }

  /** The red line after the switch: the host's refusal for the last click, if it refused. */
  error(chatId: string): string | undefined {
    return this.errors.get(chatId);
  }

  start(): void {
    if (this.started) return;
    this.started = true;
    this.deps.bus.on('workflow_settings', () => {
      void this.loadSettings();
      // The master switch and the default change what every switch says.
      for (const id of [...this.asked]) void this.load(id);
    });
    this.deps.bus.on('workflow_chat_mode', (e) => {
      const id = e.chatId;
      if (typeof id === 'string' && this.asked.has(id)) void this.load(id);
    });
    void this.loadSettings();
  }

  async loadSettings(): Promise<void> {
    try {
      this.cfg = await this.ops.settingsGet();
      this.bump();
    } catch {
      // The workflow is absent in this build: the switch stays hidden.
    }
  }

  /** Ask for a chat's switch state once (the composer calls this when it mounts). */
  ensure(chatId: string): void {
    this.start();
    if (this.asked.has(chatId)) return;
    this.asked.add(chatId);
    void this.load(chatId);
  }

  private async load(chatId: string): Promise<void> {
    try {
      this.views.set(chatId, await this.ops.modeGet(chatId));
      this.bump();
    } catch {
      // No such chat, or no workflow: leave the switch hidden.
    }
  }

  /** The user clicked. Returns the transcript line the host wrote, if any. */
  async set(chatId: string, mode: ChatMode): Promise<string | undefined> {
    try {
      const r = await this.ops.modeSet(chatId, mode);
      this.views.set(chatId, r.view);
      this.errors.delete(chatId);
      this.bump();
      return r.row;
    } catch (e) {
      this.errors.set(chatId, String((e as { message?: unknown })?.message ?? e));
      this.bump();
      return undefined;
    }
  }

  clearError(chatId: string): void {
    if (this.errors.delete(chatId)) this.bump();
  }
}

const stores = new WeakMap<object, OrchestratorStore>();

/** One store per event bus (the app's). */
export function orchestratorStoreFor(api: Api, bus: EventBus): OrchestratorStore {
  let s = stores.get(bus);
  if (!s) {
    s = new OrchestratorStore({ api, bus });
    stores.set(bus, s);
  }
  return s;
}
