// The devices store: which computers share the hub, which one's chats the sidebar shows, and the
// chat list of another computer. The choice is a host preference (`selectedDevice`), so it
// survives a reload; a choice that names a computer no longer on the hub falls back to this PC.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { deviceOps, type DeviceInfo, type DeviceOps, type DevicesList, type RemoteChatRecord, type RemoteProject } from './api.ts';
import { remoteDevice, remoteList, resolveSelected } from './model.ts';

type Listener = () => void;

export function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export interface DevicesSnapshot {
  loaded: boolean;
  /** `null` until read, and when the host has no device list (the sidebar then shows this PC). */
  list: DevicesList | null;
  /** The stored choice, as the host has it. */
  stored: string | null;
  /** The computer whose chats show: the stored choice, or this PC when that one is gone. */
  selected: string | null;
  /** The selected computer when it is not this one. */
  remote: DeviceInfo | null;
}

const EMPTY: DevicesSnapshot = { loaded: false, list: null, stored: null, selected: null, remote: null };

export interface DevicesDeps {
  api: Api;
  bus: EventBus;
}

export class DevicesStore {
  readonly ops: DeviceOps;
  private readonly deps: DevicesDeps;
  private snap: DevicesSnapshot = EMPTY;
  private readonly listeners = new Set<Listener>();
  private started = false;
  private seq = 0;
  private offs: (() => void)[] = [];

  constructor(deps: DevicesDeps) {
    this.deps = deps;
    this.ops = deviceOps(deps.api);
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): DevicesSnapshot {
    return this.snap;
  }

  private set(patch: { loaded?: boolean; list?: DevicesList | null; stored?: string | null }): void {
    const next = { ...this.snap, ...patch };
    const selected = resolveSelected(next.stored, next.list);
    this.snap = { ...next, selected, remote: remoteDevice(next.list, selected) };
    for (const l of [...this.listeners]) l();
  }

  /** Read everything once and follow the host's events. Safe to call again. */
  start(): void {
    if (this.started) return;
    this.started = true;
    void this.reload();
    this.offs = [this.deps.bus.on('devices_changed', () => void this.reload()), this.deps.bus.on('preferences', () => void this.reloadSelected())];
  }

  stop(): void {
    this.offs.forEach((o) => o());
    this.offs = [];
    this.started = false;
  }

  /** The list and the stored choice. A read that a newer one overtook is dropped. */
  async reload(): Promise<void> {
    const mine = ++this.seq;
    const [list, stored] = await Promise.allSettled([this.ops.list(), this.ops.selected()]);
    if (mine !== this.seq) return;
    this.set({
      loaded: true,
      list: list.status === 'fulfilled' ? list.value : null,
      stored: stored.status === 'fulfilled' ? stored.value : null,
    });
  }

  private async reloadSelected(): Promise<void> {
    try {
      this.set({ stored: await this.ops.selected() });
    } catch {
      /* the choice stays as it was */
    }
  }

  /** Show a computer's chats. The sidebar follows at once; a refusal puts the old choice back. */
  async select(uid: string): Promise<void> {
    const before = this.snap.stored;
    this.set({ stored: uid });
    try {
      await this.ops.select(uid);
    } catch (e) {
      this.set({ stored: before });
      throw e;
    }
  }
}

const stores = new WeakMap<Api, DevicesStore>();

/** The one store per app (keyed by its api). */
export function devicesStoreFor(api: Api, bus: EventBus): DevicesStore {
  let s = stores.get(api);
  if (!s) {
    s = new DevicesStore({ api, bus });
    stores.set(api, s);
  }
  return s;
}

// ---- another computer's chat list ----

export const REMOTE_PAGE = 200;

export interface RemoteListSnapshot {
  rows: SidebarRow[];
  projects: ProjectNode[];
  more: boolean;
  loading: boolean;
  error: string | null;
}

export interface RemoteQuery {
  deviceUid: string;
  profileId: string;
  archived: boolean;
}

const BLANK: RemoteListSnapshot = { rows: [], projects: [], more: false, loading: false, error: null };

/** The chats of one other computer for one profile, a page at a time. */
export class RemoteList {
  private snap: RemoteListSnapshot = BLANK;
  private readonly listeners = new Set<Listener>();
  private chats = new Map<string, RemoteChatRecord>();
  private projects = new Map<string, RemoteProject>();
  private query: RemoteQuery | null = null;
  private seq = 0;

  private readonly ops: Pick<DeviceOps, 'chats'>;

  constructor(ops: Pick<DeviceOps, 'chats'>) {
    this.ops = ops;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): RemoteListSnapshot {
    return this.snap;
  }

  private publish(patch: Partial<RemoteListSnapshot>, rebuild = false): void {
    const built = rebuild ? remoteList([...this.chats.values()], [...this.projects.values()]) : null;
    this.snap = { ...this.snap, ...patch, ...(built ?? {}) };
    for (const l of [...this.listeners]) l();
  }

  /** Forget everything (no other computer is showing). */
  clear(): void {
    this.seq++;
    this.query = null;
    this.chats = new Map();
    this.projects = new Map();
    this.snap = BLANK;
    for (const l of [...this.listeners]) l();
  }

  /** Read the first page again; a different query empties the list first. */
  async load(q: RemoteQuery): Promise<void> {
    const same = this.query && this.query.deviceUid === q.deviceUid && this.query.profileId === q.profileId && this.query.archived === q.archived;
    this.query = q;
    const mine = ++this.seq;
    if (!same) {
      this.chats = new Map();
      this.projects = new Map();
      this.publish({ ...BLANK, loading: true }, false);
    } else {
      this.publish({ loading: true, error: null });
    }
    try {
      const page = await this.ops.chats({ ...q, offset: 0, limit: REMOTE_PAGE });
      if (mine !== this.seq) return;
      // A refresh replaces what was held: the first page is the freshest word on it.
      this.chats = new Map(page.chats.map((c) => [c.id, c]));
      this.projects = new Map(page.projects.map((p) => [p.id, p]));
      this.publish({ more: page.more, loading: false, error: null }, true);
    } catch (e) {
      if (mine === this.seq) this.publish({ loading: false, error: errorText(e) });
    }
  }

  /** The next page ("Load more chats"). */
  async loadMore(): Promise<void> {
    const q = this.query;
    if (!q || this.snap.loading || !this.snap.more) return;
    const mine = ++this.seq;
    this.publish({ loading: true, error: null });
    try {
      const page = await this.ops.chats({ ...q, offset: this.chats.size, limit: REMOTE_PAGE });
      if (mine !== this.seq) return;
      for (const c of page.chats) this.chats.set(c.id, c);
      for (const p of page.projects) this.projects.set(p.id, p);
      this.publish({ more: page.more, loading: false, error: null }, true);
    } catch (e) {
      if (mine === this.seq) this.publish({ loading: false, error: errorText(e) });
    }
  }
}
