// The profiles store: the host's profiles view, which chats carry which label, the household
// switches, and (only while a household row is on screen) other people's shared chat lists.
//
// Labels come from the host as one chat-id list per profile other than home; a chat in none of
// them, and not started from a labelled chat, is the home profile's (the host's own rule). The
// household lists live in memory only: nothing is saved, and they refresh every 30 s while
// something watches them.

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
import { householdTargets } from './model.ts';

export const HOUSEHOLD_POLL_MS = 30_000;
/** A chat first seen this recently, while another person's profile is active, joins that profile. */
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
  /** Chat id → profile id, for every chat labelled with a profile other than home. */
  labels: ReadonlyMap<string, string>;
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
  private snap: ProfilesSnapshot = { loaded: false, view: null, labels: new Map(), sharing: OFF, people: null, groups: new Map(), error: null };
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
      this.deps.bus.on('chat', (e) => this.sawChat(e.chat as { id: string; parentChatId: string | null; sideSourceId: string | null; createdAt: number; archived: boolean })),
    ];
  }

  stop(): void {
    this.offs.forEach((o) => o());
    this.offs = [];
    this.started = false;
  }

  /** The view and the labels. A reload that a newer one overtook is dropped. */
  async reload(): Promise<void> {
    const mine = ++this.seq;
    try {
      const view = await this.ops.list();
      const others = view.profiles.filter((p) => !p.isHome);
      const lists = await Promise.all(others.map((p) => this.ops.chatIds(p.id).then((ids) => [p.id, ids] as const)));
      if (mine !== this.seq) return;
      const labels = new Map<string, string>();
      for (const [pid, ids] of lists) for (const id of ids) labels.set(id, pid);
      this.set({ loaded: true, view, labels, error: null });
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

  /** A chat the user just started while another person's profile is showing joins that profile. */
  private sawChat(c: { id: string; parentChatId: string | null; sideSourceId: string | null; createdAt: number; archived: boolean } | undefined): void {
    if (!c || this.seenChats.has(c.id)) return;
    this.seenChats.add(c.id);
    const view = this.snap.view;
    const active = view?.activeId;
    if (!view || !active || active === view.homeId) return;
    const now = (this.deps.now ?? Date.now)();
    if (c.parentChatId || c.sideSourceId || c.archived || now - c.createdAt > NEW_CHAT_WINDOW_MS) return;
    this.set({ labels: new Map(this.snap.labels).set(c.id, active) });
    this.ops.assign([c.id], active).catch(() => void this.reload());
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
