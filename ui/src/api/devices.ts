// Chats that run on another device (host crates `devices` and `chats::remote`). The list ops are in
// `../devices/api.ts`; this file holds what the chat view needs once such a chat is open: the
// device states, the error codes a refused change carries, the events, and the two extra calls.
//
// A chat of another device is read from this PC's copy, so the ordinary chat ops (`chat_get`,
// `transcript_page`, `chat_send`, `chat_stop`, `interaction_answer`, `chat_set_model`,
// `chat_rename`, `chat_archive`, `chat_restore`, `draft_save`) work on its id unchanged, and the
// host raises the same `chat`, `thread`, `message`, `pending`, `refresh` and `chat_removed`
// events for it. Every other op that names such a chat is refused with "This chat runs on {device}.
// Do that there." (`chat_warm`, `title_suggest`, `autocomplete_suggest` and `skills_list` answer
// empty instead).

import type { Api } from './client.ts';
import type { ModelCatalogue } from './types.ts';

/** How a device reads to the person (`devices_list`). */
export type DeviceState =
  /** Up, and lets other devices run its chats. */
  | 'live'
  /** Up, but its switch "Let other devices use chats on this PC" is off: the chat can be read only. */
  | 'readonly'
  /** The hub says it is not up: the copy here can be read, nothing can be sent. */
  | 'offline'
  /** This PC's own line to the hub is down: what shows is the copy here. */
  | 'reconnecting';

/**
 * The `code` of an op refused because of where the chat runs. `message` is the sentence to show.
 * `unconfirmed` means the request went out and no answer came back: the change may or may not
 * have happened, and nothing is ever sent again by itself.
 */
export type RemoteErrorCode = 'reconnecting' | 'offline' | 'not_allowed' | 'unconfirmed';

export function isRemoteError(code: string): code is RemoteErrorCode {
  return code === 'reconnecting' || code === 'offline' || code === 'not_allowed' || code === 'unconfirmed';
}

/** Events the copy raises beyond the chat events. Each is a signal: read again through `api`. */
export const DEVICE_EVENTS = {
  /** The devices, or how one reads, changed: read `devices_list`. */
  devices: 'devices_changed',
  /** The chats of one device changed (`deviceUid`): read `devices_chats`. At most every 300 ms. */
  chats: 'devices_chats_changed',
  /** The profiles changed (people the hub names were added): read `profiles_list`. */
  profiles: 'profiles_changed',
} as const;

export function remoteChatOps(api: Api) {
  return {
    /**
     * Listen to a chat of another device and wait for its first rows. Showing the chat already does
     * this in the background (`chat_get`); call it to wait, or to find out the device cannot be
     * reached. An offline device answers at once: the copy still reads.
     */
    open: (chatId: string) => api<Record<string, never>>('devices_open_chat', { chatId }),
    /** The models another device last said it offers (Claude and its local models). */
    catalogue: (deviceUid: string) => api<ModelCatalogue>('model_catalogue', { harness: 'claude', deviceUid }),
  };
}

export type RemoteChatOps = ReturnType<typeof remoteChatOps>;
