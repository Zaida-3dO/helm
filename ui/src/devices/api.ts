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
  /** Every hub person this PC acts for — one per linked profile, the PC's own first. */
  people?: { personId: string; person: string }[];
  /** The profiles on that PC, from its hello, for "Move to profile" on its chats. */
  profiles?: { profileId: string; name: string; personId?: string | null }[];
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
  /** A folder project's folder on that computer (a board has none of its own). */
  path?: string;
  /** A board's folder project: where its new chats start. Absent when none is set. */
  folderId?: string;
  pinned?: boolean;
}

/** A chat of another computer: the local record plus who it belongs to and where it runs. */
export type RemoteChatRecord = ChatRecord & {
  hostDeviceId?: string;
  /** The hub person it belongs to; null when that person has no profile here. */
  personId?: string | null;
  /** The people it is shared with. */
  sharedWith?: string[];
  /** Those shares as hub person ids (what `sharedWith` is mapped from). */
  sharedWithPeople?: string[];
  /** Manual sidebar order on its home PC, when one is set. */
  position?: number | null;
  projectName?: string;
  projectColour?: string | null;
  /** The board this chat shows under on its home PC; absent when it sits on no board. */
  boardId?: string | null;
  boardName?: string | null;
  boardColour?: string | null;
  needsAttention?: boolean;
  /** Allow cards and questions waiting on the PC that runs the chat (kept fresh in the copy). */
  waitingCards?: number;
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

/**
 * A port of the PC a chat runs on, forwarded to this PC: `localhost:<localPort>` here reaches
 * `localhost:<port>` there, through the hub line. It lives only while both PCs run.
 */
export interface Forward {
  forwardId: string;
  chatId: string;
  /** The PC the port is on. */
  deviceUid: string;
  deviceName: string;
  port: number;
  /** The port on this PC: the same one when it was free. */
  localPort: number;
  url: string;
  title: string;
  /** Connections open through it now. */
  streams: number;
  openedAt: number;
  lastUsedAt: number;
}

/** The event the host sends when a forward opens, closes or changes. */
export const FORWARDS_CHANGED = 'devices_forwards_changed';

/**
 * The address a forward opens in the browser: always plain http to this PC's loopback, built here
 * from the local port, never taken from the host's answer as written.
 */
export function forwardAddress(f: Pick<Forward, 'localPort'>): string | null {
  const p = f.localPort;
  return Number.isInteger(p) && p >= 1 && p <= 65535 ? `http://localhost:${p}/` : null;
}

/** The preference that remembers which computer's chats the sidebar shows. */
export const SELECTED_DEVICE_KEY = 'selectedDevice';

/** A `selectedDevice` value meaning every computer on the hub, this one included. */
export const ALL_DEVICES = 'all';

export function deviceOps(api: Api) {
  return {
    list: () => api<DevicesList>('devices_list', {}),
    /** One page of a computer's chats for a profile (own and shared). `deviceUid: "all"` is
     *  every computer this PC keeps a copy of; `who: "any"` widens past the profile to every
     *  person, a hub person id to that one. Both are a view, never a limit. */
    chats: (a: { deviceUid: string; profileId: string | null; who?: 'any' | string; archived?: boolean; offset?: number; limit?: number }) => api<DeviceChats>('devices_chats', a),
    /** Stage an image, a file or pasted text for the next message to a chat that runs on
     *  another PC; the id rides the send like a local attachment id. */
    attach: (a: { chatId: string; kind: 'image' | 'file' | 'text'; name: string; mime?: string; data?: string; path?: string; text?: string }) =>
      api<{ attachmentId: string }>('devices_attach', a),
    /** Take back one staged attachment of a chat that runs on another PC. */
    discard: (chatId: string, attachmentId: string) => api<unknown>('devices_discard', { chatId, attachmentId }),
    /** The projects a computer last said it has (every one, not only those holding chats shown to this profile). */
    projects: (deviceUid: string) => api<DeviceChats>('devices_chats', { deviceUid, limit: 1 }).then((r) => r.projects),
    /** Ask a computer for its projects again (it only says them when this PC connects to it); what
     *  the sidebar and the new-chat page list follows. */
    refreshProjects: (deviceUid: string) => api<DeviceChats>('devices_projects_refresh', { deviceUid }).then((r) => r.projects),
    createChat: (a: { deviceUid: string; projectId: string; model?: string; effort?: string }) => api<{ chatId: string }>('devices_create_chat', a),
    /** Make a project on that computer: a board when it has a task board, else a folder. */
    createProject: (a: { deviceUid: string; title: string; description?: string; folderProjectId?: string }) =>
      api<{ projectId: string; kind: 'folder' | 'board' }>('devices_project_create', a),
    /** Pin a project there, or give a board the folder of one of that computer's folder projects. */
    setProject: (a: { deviceUid: string; projectId: string; pinned?: boolean; folderProjectId?: string }) => api<unknown>('devices_project_set', a),
    /** Show a mirrored chat to one more person on the PC that runs it, or stop. */
    shareChat: (a: { chatId: string; personId: string; shared: boolean }) => api<unknown>('devices_share_chat', a),
    /** Hand a mirrored chat to one of the profiles on the PC that runs it. */
    moveProfile: (a: { chatId: string; profileId: string }) => api<unknown>('devices_move_profile', a),
    /** Forward a port of the PC a chat runs on to this PC (loopback only, on both ends). */
    forwardOpen: (a: { chatId: string; port: number; title?: string }) => api<Forward>('devices_forward_open', a),
    /** Every port forwarded to this PC now. */
    forwards: () => api<{ forwards: Forward[] }>('devices_forwards', {}).then((r) => (Array.isArray(r?.forwards) ? r.forwards : [])),
    forwardClose: (forwardId: string) => api<unknown>('devices_forward_close', { forwardId }),
    /** Copy a file of a chat that runs on another PC to this PC; null when the person cancelled the dialog. */
    fileSave: (chatId: string, path: string) => api<{ path: string | null }>('devices_file_save', { chatId, path }).then((r) => r?.path ?? null),
    self: () => api<ThisDevice>('device_get', {}),
    rename: (name: string) => api<ThisDevice>('device_rename', { name }),
    /** The stored choice; null when none was made (this computer). */
    selected: () => api<unknown>('preference_get', { key: SELECTED_DEVICE_KEY }).then((v) => (typeof v === 'string' && v ? v : null)),
    select: (uid: string | null) => api<unknown>('preference', { key: SELECTED_DEVICE_KEY, value: uid }),
  };
}

export type DeviceOps = ReturnType<typeof deviceOps>;
