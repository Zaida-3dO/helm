// The device ops (host crate `devices`): which computers share this household's hub, the chats of
// another computer, and this computer's own name. Typed by hand: these ops have no generated
// bindings yet. A computer's chats are read through its mirror on this PC, never from the computer
// itself.

import type { Api } from '../api/client.ts';
import type { DeviceState } from '../api/devices.ts';
import type { ChatRecord } from '../api/types.ts';

/** One computer on the hub, as `devices_list` reports it. */
export interface DeviceInfo {
  uid: string;
  name: string;
  /** The person who uses it (the hub's name for them); null when unknown. */
  person: string | null;
  online: boolean;
  /** An ISO time from the hub, or null when it never connected. */
  lastSeenAt: string | null;
  /** Whether it lets other devices run its chats. */
  run: boolean;
  isSelf: boolean;
  /** How it reads right now: live, read-only, offline, or reconnecting (this PC's line is down). */
  state?: DeviceState;
}

export interface DevicesList {
  /** This computer's uid. */
  self: string;
  devices: DeviceInfo[];
}

/** A project of another computer, as its chat list names it. */
export interface RemoteProject {
  id: string;
  name: string;
  colour: string | null;
  kind: 'folder' | 'board';
}

/** A chat of another computer: the local record plus who it belongs to and where it runs. */
export type RemoteChatRecord = ChatRecord & {
  hostDeviceId?: string;
  /** The hub person it belongs to; null when that person has no profile here. */
  personId?: string | null;
  /** The people it is shared with. */
  sharedWith?: string[];
  projectName?: string;
  projectColour?: string | null;
  needsAttention?: boolean;
};

export interface DeviceChats {
  chats: RemoteChatRecord[];
  projects: RemoteProject[];
  more: boolean;
}

export interface ThisDevice {
  uid: string;
  name: string;
}

/** The preference that remembers which computer's chats the sidebar shows. */
export const SELECTED_DEVICE_KEY = 'selectedDevice';

export function deviceOps(api: Api) {
  return {
    list: () => api<DevicesList>('devices_list', {}),
    /** One page of a computer's chats for a profile (own and shared). */
    chats: (a: { deviceUid: string; profileId: string; archived?: boolean; offset?: number; limit?: number }) => api<DeviceChats>('devices_chats', a),
    createChat: (a: { deviceUid: string; projectId: string; model?: string; effort?: string }) => api<{ chatId: string }>('devices_create_chat', a),
    self: () => api<ThisDevice>('device_get', {}),
    rename: (name: string) => api<ThisDevice>('device_rename', { name }),
    /** The stored choice; null when none was made (this computer). */
    selected: () => api<unknown>('preference_get', { key: SELECTED_DEVICE_KEY }).then((v) => (typeof v === 'string' && v ? v : null)),
    select: (uid: string | null) => api<unknown>('preference', { key: SELECTED_DEVICE_KEY, value: uid }),
  };
}

export type DeviceOps = ReturnType<typeof deviceOps>;
