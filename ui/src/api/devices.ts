// Chats that run on another device (host crates `devices` and `chats::remote`). The list ops are in
// `../devices/api.ts`; this file holds what the chat view needs once such a chat is open: the
// device states, the events, and the two extra calls. A change refused because of where the chat
// runs carries the code `reconnecting`, `offline`, `not_allowed` or `unconfirmed` (the request went
// out and no answer came back: it may or may not have happened, and nothing is sent again by
// itself), and its `message` is the sentence to show.
//
// A chat of another device is read from this PC's copy, so the ordinary chat ops (`chat_get`,
// `transcript_page`, `chat_send`, `chat_stop`, `interaction_answer`, `chat_set_model`,
// `chat_rename`, `chat_archive`, `chat_restore`, `draft_save`) work on its id unchanged, and the
// host raises the same `chat`, `thread`, `message`, `pending`, `refresh` and `chat_removed`
// events for it. The panel ops that name the chat (`chat_folder`, `file_read`, `dir_list`,
// `file_stat`, `archive_list`, `path_resolve`, `changes`, `diff`, `changes_stage`,
// `changes_unstage`, `quick_open`) answer on the PC that runs the chat, kept inside that chat's
// project folder there (a picture too big for one answer crosses in chunks, invisibly).
// Everything else that names such a chat is refused with "This chat runs on
// {device}. Do that there." (`autocomplete_suggest` answers empty instead; the Rename dialog's
// `title_suggest`, `chat_warm`, the composer's `/`-menu list, and the chat-view actions —
// including promoting or discarding a side chat — travel to the PC that runs the chat, where
// the chat's own words are.)

import type { Api } from './client.ts';
import type { AttentionKind } from './generated/AttentionKind.ts';
import type { ItemLink } from './itemlinks.ts';
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
    /** Withdraw or recover one queued message of a chat that runs on another PC. */
    submissionAction: (chatId: string, submissionId: string, action: 'withdraw' | 'recover') =>
      api<Record<string, never>>('devices_submission_action', { chatId, submissionId, action }),
    /** The PC that runs the chat's new-chat model/effort/reply-length; read, or saved with a patch. */
    composerDefaults: (
      chatId: string,
      patch?: { model?: string; effort?: string; responseVerbosity?: string },
    ) => api<{ model?: string | null; effort?: string; responseVerbosity?: string }>('devices_composer_defaults', { chatId, ...(patch ? { patch } : {}) }),
    /** Board-item links of chats that run on other PCs, read from the PCs that run them. Same
     * shape as the local `item_links_for_chats`, so the sidebar can merge the two. */
    itemLinks: (chatIds: string[]) => api<{ links: ItemLink[] }>('devices_item_links', { chatIds }),
    /** Needs-you and flag rows of chats that run on other PCs. */
    attention: (chatIds: string[]) => api<{ needsYou: { chatId: string; kind: AttentionKind; reason: string }[]; flagged: { chatId: string; kind: AttentionKind; reason: string }[] }>('devices_attention', { chatIds }),
  };
}

export type RemoteChatOps = ReturnType<typeof remoteChatOps>;
