// Typed wrappers over the chat ops. Every call still goes through api(op, args).

import type { Api } from './client.ts';
import type { ChatFindResult } from './generated/index.ts';
import type {
  AttentionSummary,
  ChatRecord,
  LocalModelInfo,
  ModelCatalogue,
  SendAction,
  SendOrigin,
  Submission,
  SubmissionAction,
  ThreadState,
  TranscriptPage,
} from './types.ts';

export interface PageQuery {
  before?: string;
  after?: string;
  around?: number;
  inclusive?: boolean;
}

export interface InteractionAnswer {
  value?: string;
  confirmed?: boolean;
  cancelled?: boolean;
  goto?: number;
}

/** What `attachment_list` answers: a chat's attachments that are not sent yet. */
export interface StagedAttachments {
  images: Array<{ id: string; name: string; mime: string; size: number; width: number; height: number }>;
  refs: Array<{ id: string; kind: 'file' | 'folder' | 'chat'; target: string; name: string }>;
}

/** The optional parts of a send. */
export interface SendExtras {
  /** Who sent it; only `composer` (the default) clears the draft. */
  origin?: SendOrigin;
  /** Ids from `attachment_put`. */
  attachments?: string[];
  /** The panel-context payload (composer sends only). */
  context?: unknown;
}

export function chatOps(api: Api) {
  return {
    create: (cwd: string, model?: string, effort?: string) => api<ChatRecord>('chat_create', { cwd, model, effort }),
    list: () => api<ChatRecord[]>('chat_list', {}),
    get: (chatId: string) => api<{ chat: ChatRecord; thread: ThreadState }>('chat_get', { chatId }),
    page: (chatId: string, q: PageQuery = {}) => api<TranscriptPage>('transcript_page', { chatId, ...q }),
    /** Find in chat over the whole stored history, one scan step (`after` = the previous `nextCursor`). */
    find: (chatId: string, query: string, includeActivity: boolean, after?: string) =>
      api<ChatFindResult>('chat_find', { chatId, query, includeActivity, ...(after ? { after } : {}) }),
    send: (chatId: string, submissionId: string, text: string, action: SendAction, draftRevision: number, extras: SendExtras = {}) =>
      api<{ submission: Submission; chat: ChatRecord }>('chat_send', {
        chatId,
        submissionId,
        text,
        action,
        draftRevision,
        ...(extras.origin ? { origin: extras.origin } : {}),
        ...(extras.attachments?.length ? { attachments: extras.attachments } : {}),
        ...(extras.context !== undefined ? { context: extras.context } : {}),
      }),
    stop: (chatId: string) => api<{ status: string }>('chat_stop', { chatId }),
    submissionAction: (submissionId: string, action: SubmissionAction) =>
      api<{ submission: Submission }>('submission_action', { submissionId, action }),
    answer: (chatId: string, requestId: string, a: InteractionAnswer) =>
      api<null>('interaction_answer', { chatId, requestId, ...a }),
    saveDraft: (chatId: string, text: string, revision: number) =>
      api<{ revision: number }>('draft_save', { chatId, text, revision }),
    setModel: (chatId: string, model?: string, effort?: string) =>
      api<ChatRecord>('chat_set_model', { chatId, model, effort }),
    /** The composer's permission-mode pick; applies from the next turn. */
    setPermissionMode: (chatId: string, mode: string) => api<{ mode: string }>('chat_set_permission_mode', { chatId, mode }),
    catalogue: (refresh = false) => api<ModelCatalogue>('model_catalogue', { harness: 'claude', refresh }),
    /** Asks the local model servers what they have (the catalogue never does); the model picker asks once each time it opens. */
    refreshLocalModels: () => api<{ models: LocalModelInfo[]; notes: string[] }>('local_models_refresh', {}),
    /** Prepares a local chat's model the moment the user starts typing, so the first send is not held up. */
    warm: (chatId: string) => api<null>('chat_warm', { chatId }),
    stopTool: (chatId: string, toolCallId: string) => api<{ stopped: boolean }>('tool_stop', { chatId, toolCallId }),
    compact: (chatId: string) => api<null>('chat_compact', { chatId }),
    /** Ask the agent for a hand-off, start a successor chat with it and archive this one.
     *  `model`/`effort` put the successor on a different model (how the model picker rolls a
     *  chat over into a model its conversation would not otherwise fit). */
    rollOver: (chatId: string, model?: string, effort?: string) =>
      api<{ started: boolean }>('chat_roll_over', { chatId, model, effort }),
    attention: () => api<AttentionSummary>('attention_get', {}),
    archive: (chatId: string) => api<ChatRecord>('chat_archive', { chatId }),
    restore: (chatId: string) => api<ChatRecord>('chat_restore', { chatId }),
    /** Hard delete; archived, idle chats only. */
    remove: (chatId: string) => api<null>('chat_delete', { chatId }),
    markUnread: (chatId: string) => api<ChatRecord>('unread_mark', { chatId }),
    clearUnread: (chatId: string, token: string) => api<ChatRecord>('unread_clear', { chatId, token }),
    ackCompletion: (chatId: string, completionId: string) => api<ChatRecord>('completion_ack', { chatId, completionId }),
    /** Open a sent message for editing. */
    editOpen: (chatId: string, messageId: number) =>
      api<{ messageId: number; revision: number; text: string }>('message_edit_open', { chatId, messageId }),
    /** Replace a sent message and everything after it; the old branch is kept. */
    resend: (chatId: string, messageId: number, revision: number, text: string) =>
      api<{ submission: Submission; chat: ChatRecord }>('message_resend', {
        chatId,
        messageId,
        revision,
        submissionId: globalThis.crypto.randomUUID(),
        text,
      }),
    /** Promote a side chat into an ordinary chat, or discard it. */
    resolveSideChat: (chatId: string, action: 'promote' | 'discard', confirmed?: boolean) =>
      api<ChatRecord | null>('side_chat_resolve', { chatId, action, ...(confirmed ? { confirmed } : {}) }),
    /** The rest of the word being typed; empty when there is nothing to show. */
    autocomplete: (chatId: string, draft: string) =>
      api<{ completion: string; retryAfterMs: number | null }>('autocomplete_suggest', {
        chatId,
        requestId: globalThis.crypto.randomUUID(),
        draft,
      }),
    /** The CLI's commands and skills for this chat; empty until it has run. `scope` says where
     * one comes from when Helm can say (`helm-skill` for one from Helm's own skills folder). */
    skills: (chatId: string) => api<{ name: string; description: string; scope?: string }[]>('skills_list', { chatId }),
    /** Store one image for the next send; `bytes` is base64. */
    attach: (chatId: string, name: string, mime: string, bytes: string) =>
      api<{ attachmentId: string }>('attachment_put', { chatId, name, mime, bytes }),
    /** A file or folder (`path`, absolute) or another chat (`chat`, its id) by reference. */
    attachRef: (chatId: string, kind: 'path' | 'chat', target: string) =>
      api<{ attachmentId: string; kind: 'file' | 'folder' | 'chat'; name: string }>('attachment_ref', { chatId, kind, target }),
    /** Pasted text kept as a file for the next send (a very long paste), attached by reference. */
    attachText: (chatId: string, name: string, text: string) =>
      api<{ attachmentId: string; kind: 'file' | 'folder' | 'chat'; name: string; target: string }>('attachment_text_put', { chatId, name, text }),
    /** The attachments this chat holds unstaged: what a reopened composer stages again. */
    stagedAttachments: (chatId: string) =>
      api<StagedAttachments>('attachment_list', { chatId }),
    /** A small cached copy of one of this chat's images (sent or staged), for a chip or bubble. */
    thumb: (chatId: string, attachmentId: string) =>
      api<{ mime: string; bytes: string }>('attachment_get', { chatId, attachmentId, thumb: true }),
    /**
     * Copy a sent message's images and files into the chat's unstaged set, so a recovered
     * draft can carry them again. The message keeps its own; the copies are new.
     */
    recoverAttachments: (chatId: string, messageId: number) =>
      api<{ images: number; refs: number }>('attachment_recover', { chatId, messageId }),
    /** Drop one unsent attachment (image or reference) the user removed from the composer. */
    discardAttachment: (chatId: string, attachmentId: string) =>
      api<{ discarded: boolean }>('attachment_discard', { chatId, attachmentId }),
    /** The native multi-file dialog (helm-shell `files_pick`); empty when cancelled. */
    pickFiles: () => api<{ paths: string[] }>('files_pick', { title: 'Attach files' }),
    /** The native folder dialog (helm-shell `folder_pick`); null when cancelled. */
    pickFolder: () => api<{ path: string | null }>('folder_pick', { title: 'Attach a folder' }),
    /** Fork a chat (whole, or at an assistant reply); a side chat quotes `excerpt`. */
    fork: (chatId: string, opts: { messageId?: number; side?: boolean; excerpt?: string } = {}) =>
      api<ChatRecord>('chat_fork', { chatId, forkId: globalThis.crypto.randomUUID(), ...opts }),
    /** User-only: mute keeps a chat out of the badge, needs-you, toasts and the tray. */
    mute: (chatId: string, muted: boolean) => api<ChatRecord>('chat_mute', { chatId, muted }),
    /** Out of the attention groups until something new happens in the chat. */
    markAddressed: (chatId: string) => api<null>('attention_address', { chatId }),
    /** Out of the attention groups until `until` (Unix ms) or new activity; null unsnoozes. */
    snooze: (chatId: string, until: number | null) => api<null>('attention_snooze', { chatId, until }),
    /** Whether the idle check is on (it is until switched off). */
    triageEnabled: async () => (await api<{ enabled?: boolean } | null>('preference_get', { key: 'taskTriage' }))?.enabled !== false,
    /** The idle check's switch, alert delay (minutes) and custom prompt ('' for the built-in one). */
    triageSettings: async () => {
      const v = await api<{ enabled?: boolean; delayMinutes?: number; prompt?: string | null } | null>('preference_get', { key: 'taskTriage' });
      return { enabled: v?.enabled !== false, delayMinutes: v?.delayMinutes ?? 15, prompt: v?.prompt ?? '' };
    },
    setTriageSettings: (s: { enabled: boolean; delayMinutes: number; prompt: string }) =>
      api<null>('preference', { key: 'taskTriage', value: { enabled: s.enabled, delayMinutes: s.delayMinutes, prompt: s.prompt.trim() || null } }),
    /** Switch the idle check; the delay and prompt already stored are kept. */
    setTriageEnabled: async (enabled: boolean) => {
      const was = await api<{ delayMinutes?: number; prompt?: string | null } | null>('preference_get', { key: 'taskTriage' });
      return api<null>('preference', { key: 'taskTriage', value: { enabled, delayMinutes: was?.delayMinutes ?? 15, prompt: was?.prompt ?? null } });
    },
  };
}

export type ChatOps = ReturnType<typeof chatOps>;

/** One picture in a gallery the agent showed. */
export interface ShownImageMeta {
  index: number;
  name: string;
  caption: string | null;
  mime: string;
  width: number;
  height: number;
}

export interface ShownGallery {
  galleryId: string;
  title: string | null;
  createdAt: number;
  images: ShownImageMeta[];
}

export interface OptionBoardOption {
  index: number;
  label: string;
  detail: string | null;
  hasImage: boolean;
}

/** What the user chose: the picked labels, or their own words in `other`. */
export interface OptionBoardAnswer {
  labels: string[];
  other: string | null;
  answeredAt: number;
}

export interface OptionBoard {
  boardId: string;
  question: string;
  multiSelect: boolean;
  allowOther: boolean;
  createdAt: number;
  options: OptionBoardOption[];
  answer: OptionBoardAnswer | null;
}

/** The galleries and option boards that agents put in a chat. A board's id also reads as a gallery id (the option's index picks the picture). */
export function cardOps(api: Api) {
  return {
    gallery: (chatId: string, galleryId: string) => api<ShownGallery>('shown_gallery_get', { chatId, galleryId }),
    image: (chatId: string, galleryId: string, index: number) => api<{ mime: string; bytes: string }>('shown_image_get', { chatId, galleryId, index }),
    thumb: (chatId: string, galleryId: string, index: number) => api<{ mime: string; bytes: string }>('shown_image_thumb', { chatId, galleryId, index }),
    board: (chatId: string, boardId: string) => api<OptionBoard>('option_board_state', { chatId, boardId }),
    /** Records the one answer: picked `labels`, or `other` (never both). A second answer is refused with code `conflict`. */
    answer: (chatId: string, boardId: string, a: { labels?: string[]; other?: string }) => api<OptionBoard>('option_board_answer', { chatId, boardId, ...a }),
  };
}

export type CardOps = ReturnType<typeof cardOps>;
