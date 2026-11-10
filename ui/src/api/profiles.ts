// Profiles, the household switches and the peer calls (host crates `profiles` and `relay`).
// Typed by hand: these ops have no generated bindings. Every profiles change answers with the
// whole view and emits `profiles_changed`.

import type { Api } from './client.ts';

export interface Profile {
  id: string;
  name: string;
  /** `#rrggbb`. */
  colour: string;
  /** The hub's id for the person this profile is, when linked. */
  hubPersonId: string | null;
  isHome: boolean;
  /** How to address this person and write for them; read by every chat on the profile. */
  about?: string;
  /** The home profile still has its placeholder name: the first-run question is asked. */
  needsName?: boolean;
}

/** A profile with the counts the popover and the profile view show (chats that are not archived). */
export interface ProfileSummary extends Profile {
  chats: number;
  running: number;
  attention: number;
}

export interface ProfilesView {
  profiles: ProfileSummary[];
  /** `null` before the first profile is made. */
  homeId: string | null;
  /** The profile the sidebar shows; `null`: all profiles. */
  activeId: string | null;
}

/** What this install lets the household see; each switch counts only while the one before is on. */
export interface Sharing {
  chats: boolean;
  transcripts: boolean;
  messages: boolean;
}

export interface PeerMachine {
  id: string;
  name: string;
  online: boolean;
  /** When it last connected: an ISO time from the hub (epoch milliseconds also read), or null if never. */
  lastSeen: string | number | null;
  sharing: Sharing;
}

export interface PeerPerson {
  id: string;
  name: string;
  /** This install's own person. */
  you: boolean;
  machines: PeerMachine[];
}

export interface PeerChat {
  chatId: string;
  title: string;
  status: string;
  projectName?: string | null;
  profileName?: string | null;
  updatedAt: number;
  needsAttention?: boolean;
}

export interface PeerChatList {
  chats: PeerChat[];
  nextOffset?: number | null;
}

export interface PeerMessage {
  messageId: number;
  role: string;
  text: string;
  at: number;
  tool?: string;
  truncated?: boolean;
}

export interface PeerTranscript {
  chat?: { title?: string; [k: string]: unknown };
  messages: PeerMessage[];
  /** Pass as `beforeMessageId` for the page before this one; absent on the first page. */
  beforeMessageId?: number;
}

export interface PeerTarget {
  personId: string;
  machineId?: string;
}

export function profilesOps(api: Api) {
  return {
    list: () => api<ProfilesView>('profiles_list', {}),
    create: (a: { name: string; colour?: string; hubPersonId?: string }) => api<ProfilesView & { profile: Profile }>('profiles_create', a),
    /** `hubPersonId: null` unlinks. */
    update: (a: { id: string; name?: string; colour?: string; hubPersonId?: string | null; about?: string }) => api<ProfilesView & { profile: Profile }>('profiles_update', a),
    remove: (id: string) => api<ProfilesView>('profiles_delete', { id }),
    setActive: (id: string | null) => api<ProfilesView>('profiles_set_active', { id }),
    assign: (chatIds: string[], profileId: string) => api<ProfilesView>('profiles_assign_chats', { chatIds, profileId }),
    /** Share chats with a profile (`shared`) or stop sharing them. At most 500 chats. */
    share: (chatIds: string[], profileId: string, shared: boolean) => api<ProfilesView>('profiles_share_chats', { chatIds, profileId, shared }),
    /** The chats a profile shows: its own and those shared with it. */
    chatIds: (profileId: string) => api<{ chatIds: string[] }>('profiles_chat_ids', { profileId }).then((r) => r.chatIds),
  };
}

export function householdOps(api: Api) {
  return {
    get: () => api<Sharing>('household_get', {}),
    set: (s: Partial<Sharing>) => api<Sharing>('household_set', s),
    people: () => api<{ people: PeerPerson[] }>('peer_people', {}).then((r) => r.people ?? []),
    listChats: (t: PeerTarget & { limit?: number; offset?: number }) => api<PeerChatList>('peer_list_chats', t),
    readChat: (t: PeerTarget & { chatId: string; beforeMessageId?: number; limit?: number }) => api<PeerTranscript>('peer_read_chat', t),
    send: (t: PeerTarget & { chatId: string; message: string }) => api<unknown>('peer_send', t),
  };
}
