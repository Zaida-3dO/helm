// The dev fake host's profiles, sharing, household switches and peer calls (the profile item and
// screen, Settings → Profiles and the household switches in a plain browser). Dev builds only.
//
// The page URL picks the state:
//   (nothing)            one home profile and one other person, no hub paired
//   ?profiles=empty      no profile yet: the profile item asks "Who uses this Helm?"
//   ?profiles=firstrun   a placeholder home profile ("Me") that still needs its name
//   ?household=on        paired to a hub; the other person's laptop is online and shares chats
//                        and transcripts (also the default for any other `household=` value)
//   ?household=send      the same, and they allow messages into their chats
//   ?household=offline   their laptop is off; its last list shows greyed
//   ?household=hidden    their laptop is on but shares nothing

import { ApiError } from '../api/client.ts';
import type { PeerChat, PeerMessage, PeerPerson, Profile, ProfilesView, Sharing } from '../api/profiles.ts';
import type { FakeOpHandler } from './fakeHost.ts';

type HouseholdMode = 'none' | 'on' | 'send' | 'offline' | 'hidden';

function param(name: string): string | null {
  return typeof location === 'undefined' ? null : new URLSearchParams(location.search).get(name);
}

function householdMode(): HouseholdMode {
  const v = param('household');
  if (v === null) return 'none';
  return v === 'send' || v === 'offline' || v === 'hidden' ? v : 'on';
}

const MIN = 60_000;
const COLOURS = ['#6d8cff', '#ff8a65', '#4db6ac', '#ba68c8', '#ffd54f', '#81c784', '#f06292', '#90a4ae'];

export function profilesFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const mode = householdMode();
  const t0 = Date.now();
  const profiles: Profile[] =
    param('profiles') === 'empty'
      ? []
      : param('profiles') === 'firstrun'
        ? [{ id: 'prof-home', name: 'Me', colour: COLOURS[0]!, hubPersonId: null, isHome: true, about: '', needsName: true }]
        : [
            { id: 'prof-home', name: 'Alex', colour: COLOURS[0]!, hubPersonId: 'p-self', isHome: true, about: '', needsName: false },
            { id: 'prof-sam', name: 'Sam', colour: COLOURS[1]!, hubPersonId: 'p-other', isHome: false, about: '', needsName: false },
          ];
  // The host always has an active profile; the home profile unless one was chosen.
  let active: string | null = profiles.find((p) => p.isHome)?.id ?? null;
  const labels = new Map<string, string>();
  /** Chats shared with a profile other than their owner's: chat id → profile ids. */
  const shares = new Map<string, Set<string>>();
  /** Chats the fake has seen, so a chat started later joins the profile showing (the host's insert rule). */
  const known = new Set<string>();
  let seeded = false;
  const sharing: Sharing = { chats: false, transcripts: false, messages: false };
  let seq = 1;

  const normalised = (): Sharing => {
    const transcripts = sharing.chats && sharing.transcripts;
    return { chats: sharing.chats, transcripts, messages: transcripts && sharing.messages };
  };

  const people = (): PeerPerson[] => {
    const other: Sharing =
      mode === 'hidden' ? { chats: false, transcripts: false, messages: false } : { chats: true, transcripts: true, messages: mode === 'send' };
    return [
      { id: 'p-self', name: 'Alex', you: true, machines: [{ id: 'm1', name: 'Desk PC', online: true, lastSeen: new Date().toISOString(), sharing: normalised() }] },
      {
        id: 'p-other',
        name: 'Sam',
        you: false,
        machines: [{ id: 'm2', name: "Sam's laptop", online: mode !== 'offline', lastSeen: new Date(mode === 'offline' ? t0 - 130 * MIN : Date.now()).toISOString(), sharing: other }],
      },
    ];
  };

  const theirChats = (): PeerChat[] => [
    { chatId: 'r1', title: 'Plan the shop banner', status: 'idle', projectName: 'Shop', profileName: 'Sam', updatedAt: Date.now() - 12 * MIN, needsAttention: true },
    { chatId: 'r2', title: 'Order spreadsheet', status: 'running', projectName: 'Shop', profileName: 'Sam', updatedAt: Date.now() - 90 * MIN },
    { chatId: 'r3', title: 'Birthday card ideas', status: 'idle', projectName: null, profileName: 'Sam', updatedAt: Date.now() - 26 * 60 * MIN },
  ];

  const transcript = (): PeerMessage[] =>
    Array.from({ length: 12 }, (_, i) => ({
      messageId: i + 1,
      role: i % 2 === 0 ? 'user' : 'assistant',
      text: i % 2 === 0 ? `Question ${i / 2 + 1} about the shop banner.` : `Here is an answer to question ${(i + 1) / 2}, with a couple of options to look at.`,
      at: Date.now() - (12 - i) * MIN,
    }));

  const view = (ctx: { call: (op: string, a?: Record<string, unknown>) => unknown }): ProfilesView => {
    adopt(ctx);
    const chats = ((ctx.call('chat_list', {}) as { id: string; archived?: boolean; status?: string }[]) ?? []).filter((c) => !c.archived);
    const home = profiles.find((p) => p.isHome);
    const of = (c: { id: string }) => labels.get(c.id) ?? home?.id;
    return {
      profiles: profiles.map((p) => {
        const mine = chats.filter((c) => of(c) === p.id);
        return { ...p, chats: mine.length, running: mine.filter((c) => c.status === 'running').length, attention: 0 };
      }),
      homeId: home?.id ?? null,
      activeId: active,
    };
  };

  /** A top-level chat that appeared after the first look belongs to the profile showing. */
  const adopt = (ctx: { call: (op: string, a?: Record<string, unknown>) => unknown }): void => {
    const all = (ctx.call('chat_list', {}) as { id: string; parentChatId?: string | null }[]) ?? [];
    for (const c of all) {
      if (known.has(c.id)) continue;
      known.add(c.id);
      const home = profiles.find((p) => p.isHome);
      if (seeded && !c.parentChatId && active && active !== home?.id) labels.set(c.id, active);
    }
    // A first look at a seeded dev host: one chat is the other person's, one is shared with them.
    const other = profiles.find((p) => !p.isHome);
    if (!seeded && other && all.length >= 3) {
      labels.set(all[1]!.id, other.id);
      shares.set(all[2]!.id, new Set([other.id]));
    }
    seeded = true;
  };

  const must = (id: unknown): Profile => {
    const p = profiles.find((x) => x.id === id);
    if (!p) throw new ApiError('not_found', 'There is no profile with that id.');
    return p;
  };
  const cleanName = (raw: unknown): string => {
    const n = String(raw ?? '').trim();
    if (!n || n.length > 40) throw new ApiError('bad_args', "A profile's name is 1 to 40 characters.");
    if (profiles.some((p) => p.name.toLowerCase() === n.toLowerCase())) throw new ApiError('bad_args', 'Another profile already has that name.');
    return n;
  };
  const cleanColour = (raw: unknown): string => {
    const c = String(raw ?? '').trim();
    if (!/^#[0-9a-fA-F]{6}$/.test(c)) throw new ApiError('bad_args', 'A colour looks like #6d8cff.');
    return c.toLowerCase();
  };

  const handler: FakeOpHandler = (op, a, ctx) => {
    const changed = (extra: Record<string, unknown> = {}) => {
      ctx.emit({ kind: 'profiles_changed' });
      return { ...view(ctx), ...extra };
    };
    switch (op) {
      case 'profiles_list':
        return view(ctx);
      case 'profiles_create': {
        const profile: Profile = {
          id: `prof-${++seq}`,
          name: cleanName(a.name),
          colour: a.colour ? cleanColour(a.colour) : COLOURS[profiles.length % COLOURS.length]!,
          hubPersonId: typeof a.hubPersonId === 'string' && a.hubPersonId.trim() ? a.hubPersonId.trim() : null,
          isHome: profiles.length === 0,
        };
        profiles.push(profile);
        if (profile.isHome) active = profile.id;
        return changed({ profile });
      }
      case 'profiles_update': {
        const p = must(a.id);
        if (typeof a.name === 'string' && a.name.trim().toLowerCase() !== p.name.toLowerCase()) p.name = cleanName(a.name);
        if (typeof a.colour === 'string') p.colour = cleanColour(a.colour);
        if ('hubPersonId' in a) p.hubPersonId = typeof a.hubPersonId === 'string' && a.hubPersonId.trim() ? a.hubPersonId.trim() : null;
        if (typeof a.about === 'string') p.about = a.about.slice(0, 2000);
        if (typeof a.name === 'string' && a.name.trim()) p.needsName = false;
        return changed({ profile: p });
      }
      case 'profiles_delete': {
        const p = must(a.id);
        if (p.isHome) throw new ApiError('bad_args', 'The home profile cannot be deleted.');
        profiles.splice(profiles.indexOf(p), 1);
        for (const [chat, prof] of [...labels]) if (prof === p.id) labels.delete(chat);
        if (active === p.id) active = profiles.find((x) => x.isHome)?.id ?? null;
        for (const set of shares.values()) set.delete(p.id);
        return changed();
      }
      case 'profiles_set_active':
        active = a.id === null || a.id === undefined ? null : must(a.id).id;
        return changed();
      case 'profiles_assign_chats': {
        const p = must(a.profileId);
        for (const id of (a.chatIds as string[]) ?? []) {
          if (p.isHome) labels.delete(id);
          else labels.set(id, p.id);
        }
        return changed();
      }
      case 'profiles_share_chats': {
        const p = must(a.profileId);
        for (const id of (a.chatIds as string[]) ?? []) {
          const set = shares.get(id) ?? new Set<string>();
          if (a.shared === false) set.delete(p.id);
          else set.add(p.id);
          shares.set(id, set);
        }
        return changed();
      }
      case 'profiles_chat_ids': {
        // Own and shared, as the host reads its column.
        adopt(ctx);
        const p = must(a.profileId);
        const all = ((ctx.call('chat_list', {}) as { id: string }[]) ?? []).map((c) => c.id);
        const home = profiles.find((x) => x.isHome)?.id;
        const owner = (id: string) => labels.get(id) ?? home;
        return { chatIds: all.filter((id) => owner(id) === p.id || shares.get(id)?.has(p.id)) };
      }
      case 'household_get':
        return normalised();
      case 'household_set':
        for (const k of ['chats', 'transcripts', 'messages'] as const) if (typeof a[k] === 'boolean') sharing[k] = a[k] as boolean;
        ctx.emit({ kind: 'household_changed' });
        return normalised();
      case 'peer_people':
        return { people: people() };
      case 'peer_list_chats': {
        const m = people().find((p) => p.id === a.personId)?.machines[0];
        if (!m?.online) throw new ApiError('unavailable', 'That computer is not connected to the hub.');
        if (!m.sharing.chats) throw new ApiError('unavailable', 'Sam does not share their chat list with the household.');
        return { chats: theirChats() };
      }
      case 'peer_read_chat': {
        const m = people().find((p) => p.id === a.personId)?.machines[0];
        if (!m?.online) throw new ApiError('unavailable', 'That computer is not connected to the hub.');
        if (!m.sharing.transcripts) throw new ApiError('unavailable', 'Sam does not let the household read their transcripts.');
        const limit = Math.min(Number(a.limit ?? 20), 50);
        const before = typeof a.beforeMessageId === 'number' ? a.beforeMessageId : Number.MAX_SAFE_INTEGER;
        const older = transcript().filter((x) => x.messageId < before);
        const page = older.slice(-limit);
        return { chat: { title: 'Plan the shop banner' }, messages: page, ...(page.length === limit && older.length > limit ? { beforeMessageId: page[0]!.messageId } : {}) };
      }
      case 'peer_send': {
        const m = people().find((p) => p.id === a.personId)?.machines[0];
        if (!m?.sharing.messages) throw new ApiError('unavailable', 'Sam does not let the household message their chats.');
        return { sent: true };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['profiles_', 'household_', 'peer_'], handler };
}
