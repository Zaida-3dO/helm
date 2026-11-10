// Profiles (host crate `profiles`) and the people on the hub a profile can be linked to (host
// crate `relay`). Typed by hand: these ops have no generated bindings. Every profiles change
// answers with the whole view and emits `profiles_changed`.

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

/** A person on the hub, as a profile's "Hub person" choice lists them. */
export interface HubPerson {
  id: string;
  name: string;
  /** This install's own person. */
  you: boolean;
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
    /** Everyone on the hub, for linking a profile to a person. */
    hubPeople: () => api<{ people: HubPerson[] }>('peer_people', {}).then((r) => r.people ?? []),
  };
}
