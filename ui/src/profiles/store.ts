// The profiles store: the host's profiles view, the chats the active profile shows, the household
// switches, and (only while something watches them) other people's shared chat lists.
//
// The chats the active profile shows come from the host as one id list (its own and those shared
// with it), read when the profile changes and kept current from chat events. The household lists
// live in memory only: nothing is saved, and they refresh every 30 s while something watches them.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import {
  householdOps,
  profilesOps,
  type PeerChat,
  type PeerPerson,
  type ProfilesView,
  type Sharing,
} from '../api/profiles.ts';
import { hubOps } from '../api/hub.ts';
import { belongsTo, householdTargets } from './model.ts';

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

export const HOUSEHOLD_POLL_MS = 30_000;
/** On a host that does not say which profile a chat has: one first seen this recently joins the active profile. */
export const NEW_CHAT_WINDOW_MS = 15_000;

/** One machine of another person, as last read. */
export interface HouseholdGroup {
  key: string;
  personId: string;
  personName: string;
  machineId: string;
  machineName: string;
  online: boolean;
  lastSeen: number | null;
  sharing: Sharing;
  chats: PeerChat[];
  /** Why the list could not be read, in the host's plain words. */
  note: string | null;
}

export interface ProfilesSnapshot {
  loaded: boolean;
  view: ProfilesView | null;
  /** The chats the active profile shows, own and shared; `null` until read, and with no active profile. */
  ids: ReadonlySet<string> | null;
  /** Chats shared from this window, by chat id, until the host's rows say so themselves. */
  shared: ReadonlyMap<string, ReadonlySet<string>>;
  sharing: Sharing;
  /** Everyone on the hub; `null` until read, and while the hub cannot be reached. */
  people: PeerPerson[] | null;
  groups: ReadonlyMap<string, HouseholdGroup>;
  error: string | null;
}

type Listener = () => void;

const OFF: Sharing = { chats: false, transcripts: false, messages: false };

export function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export interface ProfilesDeps {
  api: Api;
  bus: EventBus;
  now?: () => number;
  setInterval?: (f: () => void, ms: number) => unknown;
  clearInterval?: (h: unknown) => void;
}

export class ProfilesStore {
  readonly ops: ReturnType<typeof profilesOps>;
  readonly household: ReturnType<typeof householdOps>;
  private readonly hub: ReturnType<typeof hubOps>;
  private readonly deps: ProfilesDeps;
  private snap: ProfilesSnapshot = { loaded: false, view: null, ids: null, shared: new Map(), sharing: OFF, people: null, groups: new Map(), error: null };
  private readonly listeners = new Set<Listener>();
  private started = false;
  private seq = 0;
  private watchers = 0;
  private timer: unknown;
  private readonly seenChats = new Set<string>();
  private offs: (() => void)[] = [];

  constructor(deps: ProfilesDeps) {
    this.deps = deps;
    this.ops = profilesOps(deps.api);
    this.household = householdOps(deps.api);
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
    void this.reloadSharing();
    this.offs = [
      this.deps.bus.on('profiles_changed', () => void this.reload()),
      this.deps.bus.on('household_changed', () => void this.reloadSharing()),
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

  private async reloadSharing(): Promise<void> {
    try {
      this.set({ sharing: await this.household.get() });
    } catch {
      /* the switches stay as they were */
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

  // ---- the household ----

  /** Start polling the household while something is showing it; call the result to stop. */
  watchHousehold(): () => void {
    this.watchers++;
    if (this.watchers === 1) {
      void this.refreshHousehold();
      const every = this.deps.setInterval ?? ((f, ms) => setInterval(f, ms));
      this.timer = every(() => void this.refreshHousehold(), HOUSEHOLD_POLL_MS);
    }
    let done = false;
    return () => {
      if (done) return;
      done = true;
      this.watchers--;
      if (this.watchers === 0) {
        (this.deps.clearInterval ?? ((h) => clearInterval(h as ReturnType<typeof setInterval>)))(this.timer);
        this.timer = undefined;
      }
    };
  }

  /** Who is on the hub, and every other person's shared chat list for the machines that are online. */
  async refreshHousehold(): Promise<void> {
    let people: PeerPerson[];
    try {
      const conn = await this.hub.connection();
      if (conn.state !== 'online') {
        this.set({ people: null, groups: new Map() });
        return;
      }
      people = await this.household.people();
    } catch {
      this.set({ people: null });
      return;
    }
    this.set({ people });
    // Every machine of every other person, whichever profile shows: switching profiles then
    // needs no new reads. The cache is dropped for machines that are gone.
    const targets = householdTargets({ profiles: [], homeId: null, activeId: null }, people);
    const built = await Promise.all(
      targets.map(async (t): Promise<HouseholdGroup> => {
        const prev = this.snap.groups.get(t.key);
        const g: HouseholdGroup = {
          key: t.key,
          personId: t.person.id,
          personName: t.person.name,
          machineId: t.machine.id,
          machineName: t.machine.name,
          online: t.machine.online,
          lastSeen: t.machine.lastSeen,
          sharing: t.machine.sharing,
          chats: prev?.chats ?? [],
          note: null,
        };
        if (t.machine.online) {
          try {
            g.chats = (await this.household.listChats({ personId: t.person.id, machineId: t.machine.id, limit: 50 })).chats ?? [];
          } catch (e) {
            g.chats = [];
            g.note = errorText(e);
          }
        }
        return g;
      }),
    );
    this.set({ groups: new Map(built.map((g) => [g.key, g])) });
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
