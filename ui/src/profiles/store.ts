// The profiles store: the host's profiles view, the chats the active profile shows, and (when
// Settings asks) the people on the hub a profile can be linked to.
//
// The chats the active profile shows come from the host as one id list (its own and those shared
// with it), read when the profile changes and kept current from chat events.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { profilesOps, type HubPerson, type ProfilesView } from '../api/profiles.ts';
import { hubOps } from '../api/hub.ts';
import { belongsTo } from './model.ts';

/** The chat record fields the store reads off a `chat` event. */
interface ChatSighting {
  id: string;
  parentChatId: string | null;
  sideSourceId: string | null;
  createdAt: number;
  archived: boolean;
  profileId?: string;
  sharedWith?: string[];
}

/** On a host that does not say which profile a chat has: one first seen this recently joins the active profile. */
export const NEW_CHAT_WINDOW_MS = 15_000;

export interface ProfilesSnapshot {
  loaded: boolean;
  view: ProfilesView | null;
  /** The chats the active profile shows, own and shared; `null` until read, and with no active profile. */
  ids: ReadonlySet<string> | null;
  /** Chats shared from this window, by chat id, until the host's rows say so themselves. */
  shared: ReadonlyMap<string, ReadonlySet<string>>;
  /** Everyone on the hub; `null` until read, and while the hub cannot be reached. */
  people: HubPerson[] | null;
  error: string | null;
}

type Listener = () => void;

export function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export interface ProfilesDeps {
  api: Api;
  bus: EventBus;
  now?: () => number;
}

export class ProfilesStore {
  readonly ops: ReturnType<typeof profilesOps>;
  private readonly hub: ReturnType<typeof hubOps>;
  private readonly deps: ProfilesDeps;
  private snap: ProfilesSnapshot = { loaded: false, view: null, ids: null, shared: new Map(), people: null, error: null };
  private readonly listeners = new Set<Listener>();
  private started = false;
  private seq = 0;
  private readonly seenChats = new Set<string>();
  private offs: (() => void)[] = [];

  constructor(deps: ProfilesDeps) {
    this.deps = deps;
    this.ops = profilesOps(deps.api);
    this.hub = hubOps(deps.api);
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get snapshot(): ProfilesSnapshot {
    return this.snap;
  }

  private set(patch: Partial<ProfilesSnapshot>): void {
    this.snap = { ...this.snap, ...patch };
    for (const l of [...this.listeners]) l();
  }

  /** Read everything once and follow the host's events. Safe to call again. */
  start(): void {
    if (this.started) return;
    this.started = true;
    void this.reload();
    this.offs = [
      this.deps.bus.on('profiles_changed', () => void this.reload()),
      this.deps.bus.on('chat', (e) => this.sawChat(e.chat as ChatSighting | undefined)),
    ];
  }

  stop(): void {
    this.offs.forEach((o) => o());
    this.offs = [];
    this.started = false;
  }

  /** The view and the active profile's chats. A reload that a newer one overtook is dropped. */
  async reload(): Promise<void> {
    const mine = ++this.seq;
    try {
      const view = await this.ops.list();
      const ids = view.activeId ? new Set(await this.ops.chatIds(view.activeId)) : null;
      if (mine !== this.seq) return;
      this.set({ loaded: true, view, ids, error: null });
    } catch (e) {
      if (mine === this.seq) this.set({ loaded: true, error: errorText(e) });
    }
  }

  /** A view an op answered with, applied now (the `profiles_changed` event then confirms it). */
  apply(view: ProfilesView): void {
    this.set({ view });
    void this.reload();
  }

  /**
   * A chat record from the `chat` event keeps the active profile's list current: the record says
   * whose the chat is and who it is shared with. A host that does not say still gets its new
   * chats right: one started from a chat in the list, or just started at the top level, joins it.
   */
  private sawChat(c: ChatSighting | undefined): void {
    const active = this.snap.view?.activeId;
    const ids = this.snap.ids;
    if (!c || !active || !ids) return;
    let member = belongsTo(c, active);
    if (member === null) {
      if (this.seenChats.has(c.id)) return;
      this.seenChats.add(c.id);
      const now = (this.deps.now ?? Date.now)();
      const fresh = !c.sideSourceId && !c.archived && now - c.createdAt <= NEW_CHAT_WINDOW_MS;
      member = (!!c.parentChatId && ids.has(c.parentChatId)) || (fresh && !c.parentChatId);
      if (!member) return;
    }
    if (member === ids.has(c.id)) return;
    const next = new Set(ids);
    if (member) next.add(c.id);
    else next.delete(c.id);
    this.set({ ids: next });
  }

  /**
   * Share one chat with a profile (or stop). `current` is who it was shared with: the marks show
   * the new set at once, and the host's event confirms it.
   */
  async share(chatId: string, profileId: string, shared: boolean, current: ReadonlySet<string>): Promise<void> {
    const view = await this.ops.share([chatId], profileId, shared);
    const set = new Set(current);
    if (shared) set.add(profileId);
    else set.delete(profileId);
    this.set({ shared: new Map(this.snap.shared).set(chatId, set) });
    this.apply(view);
  }

  /** Who is on the hub, read once when Settings shows the profiles; none while the hub is not connected. */
  async loadPeople(): Promise<void> {
    try {
      const conn = await this.hub.connection();
      this.set({ people: conn.state === 'online' ? await this.ops.hubPeople() : null });
    } catch {
      this.set({ people: null });
    }
  }
}

const stores = new WeakMap<Api, ProfilesStore>();

/** The one store per app (keyed by its api). */
export function profilesStoreFor(api: Api, bus: EventBus): ProfilesStore {
  let s = stores.get(api);
  if (!s) {
    s = new ProfilesStore({ api, bus });
    stores.set(api, s);
  }
  return s;
}
