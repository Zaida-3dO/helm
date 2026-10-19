// The new-chat page's state: one draft, held here and not on
// the host. Nothing is created until the first send, so leaving an untouched draft leaves nothing
// behind. Plain TypeScript, tested under node:test; `NewChatHost` connects it to the app.
//
// The draft is a stand-in chat record for the ordinary composer. Its ops (`draftOps`) keep the text,
// model, effort and pasted images here; the first send creates the real chat, moves the images
// onto it, sends, and opens it.

import { ApiError, type Api } from '../api/client.ts';
import type { ChatOps, SendExtras } from '../api/ops.ts';
import type { ProjectNode } from '../api/projects.ts';
import type { ChatRecord, SendAction, Submission } from '../api/types.ts';
import { folderProjectFor, pushRecent, readRecents, resolveFolder, type ResolvedFolder } from './places.ts';

export interface NewChatOptions {
  /** Preselect this project (any sidebar group). */
  projectId?: string;
  /** Start in this folder. */
  folder?: string;
}

export interface LocalImage {
  name: string;
  mime: string;
  /** Base64, as `attachment_put` takes it. */
  bytes: string;
}

export interface Draft {
  /** A stand-in id for the composer; never sent to the host. */
  id: string;
  projectId: string | null;
  /** The folder the user picked; null follows the project (folder resolution). */
  folder: string | null;
  text: string;
  revision: number;
  model: string | null;
  effort: string | null;
  images: Map<string, LocalImage>;
  createdAt: number;
}

/** What the controller needs from the app. `NewChatHost` builds it from the shell. */
export interface NewChatEnv {
  api: Api;
  ops: ChatOps;
  /** The open chat, if any. */
  selected(): string | null;
  select(chatId: string | null): void;
  /** Calls back on every selection change; returns the unsubscribe. */
  onSelection(f: () => void): () => void;
  /** The open chat's folder and project, when the chat store knows the chat. */
  chatPlace(chatId: string): { cwd: string; projectId: string } | null;
  /** Hand a created chat to the chat store, so the chat view has it at once. */
  storeChat(chat: ChatRecord): void;
  storage: Pick<Storage, 'getItem' | 'setItem'>;
  now(): number;
  uid(): string;
}

export const DRAFT_PREFIX = 'draft:';
export const LAST_PROJECT_KEY = 'helm.newchat.lastProject';

type Listener = () => void;
export type ReadyListener = (place: { projectId: string | null; folder: string | null }) => void;

export class NewChatController {
  private env: NewChatEnv | null = null;
  private pendingOpen: NewChatOptions | null = null;
  private readonly listeners = new Set<Listener>();
  private readonly ready = new Set<ReadyListener>();
  private offSelection: (() => void) | null = null;
  private lastSelected: string | null = null;
  private sending: Promise<{ submission: Submission; chat: ChatRecord }> | null = null;
  /** A chat the first send created but could not send to; the next try reuses it. */
  private created: ChatRecord | null = null;

  open = false;
  draft: Draft | null = null;
  projects: ProjectNode[] = [];
  /** The folder of the chat that was open when the page opened (folder resolution step 4). */
  openChatFolder: string | null = null;
  version = 0;

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  getVersion = (): number => this.version;

  private changed(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  /** Connect to the app. An `open` made before this is applied now. */
  attach(env: NewChatEnv): () => void {
    this.env = env;
    this.lastSelected = env.selected();
    this.offSelection = env.onSelection(() => {
      const now = env.selected();
      if (now === this.lastSelected) return;
      this.lastSelected = now;
      // Opening any chat leaves the page.
      if (now !== null && this.open) this.leave();
    });
    if (this.pendingOpen) {
      const o = this.pendingOpen;
      this.pendingOpen = null;
      this.show(o);
    }
    return () => {
      this.offSelection?.();
      this.offSelection = null;
      if (this.env === env) this.env = null;
    };
  }

  setProjects(list: ProjectNode[]): void {
    this.projects = list;
    this.changed();
  }

  /** The `chat.new` command (Ctrl+N), the sidebar's New chat and a project's "+". */
  show(opts: NewChatOptions = {}): void {
    const env = this.env;
    if (!env) {
      this.pendingOpen = opts;
      return;
    }
    const from = env.selected();
    const place = from ? env.chatPlace(from) : null;
    if (from) this.openChatFolder = place?.cwd ?? this.openChatFolder;
    const explicit = opts.projectId !== undefined || opts.folder !== undefined;
    if (!this.draft) {
      this.draft = this.newDraft(env, explicit ? null : (place?.projectId ?? this.lastProject(env)));
    } else if (!explicit && place && this.untouched()) {
      // An untouched draft is reused, and follows the chat the user came from.
      this.draft.projectId = place.projectId;
      this.draft.folder = null;
    }
    if (explicit) this.move(opts.projectId ?? this.projectForFolder(opts.folder) ?? null, opts.folder ?? null, false);
    this.open = true;
    this.lastSelected = null;
    if (from) env.select(null);
    this.changed();
    this.announce();
  }

  /** Close the page. An untouched draft is discarded; a typed one waits for the next New chat. */
  leave(): void {
    if (!this.open) return;
    this.open = false;
    if (this.untouched()) {
      this.draft = null;
      this.created = null;
    }
    this.changed();
  }

  /** Nothing typed and nothing pasted (a model or effort change alone does not count). */
  untouched(): boolean {
    const d = this.draft;
    return !d || (d.text.trim() === '' && d.images.size === 0);
  }

  /** Change the draft's project; the folder follows the new project unless `folder` is given. */
  move(projectId: string | null, folder: string | null = null, notify = true): void {
    if (!this.draft) return;
    this.draft.projectId = projectId;
    this.draft.folder = folder;
    if (this.created && !this.matchesCreated()) this.created = null;
    if (notify) {
      this.changed();
      this.announce();
    }
  }

  setFolder(folder: string): void {
    if (!this.draft) return;
    this.draft.folder = folder;
    if (this.created && !this.matchesCreated()) this.created = null;
    this.changed();
    this.announce();
  }

  place(): ResolvedFolder {
    return resolveFolder(this.draft?.projectId ?? null, this.draft?.folder ?? null, this.projects, this.openChatFolder);
  }

  /** The stand-in record the composer renders. */
  record(): ChatRecord | null {
    const d = this.draft;
    if (!d) return null;
    const folder = this.place().folder ?? '';
    return {
      id: d.id,
      projectId: d.projectId ?? '',
      cwd: folder,
      title: 'New chat',
      status: 'idle',
      error: null,
      harness: 'claude',
      model: d.model,
      effort: d.effort,
      draft: d.text,
      draftRevision: d.revision,
      createdAt: d.createdAt,
      updatedAt: d.createdAt,
      revision: this.version,
      pinned: false,
      archived: false,
      parentChatId: null,
      sideSourceId: null,
      titleState: 'fallback',
      unread: { markToken: null, completionId: null },
      muted: false,
      lastActivityAt: d.createdAt,
      stalled: false,
    };
  }

  // ---- What the composer's ops do on a draft -------------------------------------------------

  saveText(text: string, revision: number): void {
    const d = this.draft;
    if (!d || revision <= d.revision) return;
    const wasUntouched = this.untouched();
    d.text = text;
    d.revision = revision;
    // Re-render only when the page's "untouched" state flips (nothing on screen shows the text).
    if (wasUntouched !== this.untouched()) this.changed();
  }

  setModel(model?: string, effort?: string): ChatRecord {
    const d = this.draft;
    if (d) {
      if (model !== undefined) d.model = model;
      if (effort !== undefined) d.effort = effort;
      this.changed();
    }
    const r = this.record();
    if (!r) throw new ApiError('not_found', 'The new chat was closed.');
    return r;
  }

  addImage(img: LocalImage): string {
    const d = this.draft;
    if (!d) throw new ApiError('not_found', 'The new chat was closed.');
    const id = `${DRAFT_PREFIX}img:${this.env?.uid() ?? String(d.images.size)}`;
    d.images.set(id, img);
    return id;
  }

  /**
   * The first send: create the chat in the chosen folder and project, move the pasted images onto
   * it, send, then open it. A second press while this runs gets the same promise. When the send is
   * refused after the chat was made, the next try sends to that chat rather than making another.
   */
  firstSend(submissionId: string, text: string, action: SendAction, extras: SendExtras = {}): Promise<{ submission: Submission; chat: ChatRecord }> {
    if (this.sending) return this.sending;
    const run = this.doFirstSend(submissionId, text, action, extras);
    this.sending = run;
    const clear = () => {
      if (this.sending === run) this.sending = null;
    };
    run.then(clear, clear);
    return run;
  }

  private async doFirstSend(submissionId: string, text: string, action: SendAction, extras: SendExtras): Promise<{ submission: Submission; chat: ChatRecord }> {
    const env = this.env;
    const d = this.draft;
    if (!env || !d) throw new ApiError('not_found', 'The new chat was closed.');
    const { folder } = this.place();
    if (!folder) throw new ApiError('needs_folder', 'Choose a folder for this chat first.');
    let chat = this.created;
    if (!chat) {
      chat = await env.ops.create(folder, d.model ?? undefined, d.effort ?? undefined);
      this.created = chat;
      env.storeChat(chat);
      // Shown under the chosen project when that is not the folder's own project.
      const group = d.projectId;
      if (group && group !== chat.projectId && this.projects.some((p) => p.id === group)) {
        await env.api('chat_organize', { chatId: chat.id, groupProjectId: group, parentChatId: null }).catch(() => undefined);
      }
    }
    const ids: string[] = [];
    for (const id of extras.attachments ?? []) {
      const img = d.images.get(id);
      if (!img) {
        ids.push(id);
        continue;
      }
      const r = await env.ops.attach(chat.id, img.name, img.mime, img.bytes);
      d.images.delete(id);
      ids.push(r.attachmentId);
    }
    const result = await env.ops.send(chat.id, submissionId, text, action, 0, { ...extras, attachments: ids });
    env.storage.setItem(LAST_PROJECT_KEY, d.projectId ?? chat.projectId);
    pushRecent(env.storage, folder);
    this.draft = null;
    this.created = null;
    this.open = false;
    this.lastSelected = chat.id;
    env.select(chat.id);
    this.changed();
    return result;
  }

  // ---- The page's prewarm hook (for a later speed-up) ---------------------------------------

  /** Called with the page's project and folder whenever it opens or they change. */
  onReady(f: ReadyListener): () => void {
    this.ready.add(f);
    return () => {
      this.ready.delete(f);
    };
  }

  private announce(): void {
    if (!this.open) return;
    const place = { projectId: this.draft?.projectId ?? null, folder: this.place().folder };
    for (const f of [...this.ready]) {
      try {
        f(place);
      } catch {
        // A listener's failure never stops the page.
      }
    }
  }

  recents(): string[] {
    return this.env ? readRecents(this.env.storage) : [];
  }

  // ---- helpers -----------------------------------------------------------------------------------

  private newDraft(env: NewChatEnv, projectId: string | null): Draft {
    return { id: `${DRAFT_PREFIX}${env.uid()}`, projectId, folder: null, text: '', revision: 0, model: null, effort: null, images: new Map(), createdAt: env.now() };
  }

  private lastProject(env: NewChatEnv): string | null {
    const id = env.storage.getItem(LAST_PROJECT_KEY);
    return id && this.projects.some((p) => p.id === id && !p.archived) ? id : null;
  }

  /** The first open folder project over `folder`, if any. */
  private projectForFolder(folder: string | undefined): string | null {
    if (!folder) return null;
    return this.projects.find((p) => folderProjectFor(p.id, folder, this.projects) !== null)?.id ?? null;
  }

  private matchesCreated(): boolean {
    const c = this.created;
    const folder = this.place().folder;
    return !!c && !!folder && folder === c.cwd;
  }
}

/** The app's one controller. */
export const newChat = new NewChatController();

/**
 * The composer's ops for a draft: the text, model, effort and images stay in the controller, and
 * the first send creates the chat. Everything else goes to the real ops.
 */
export function draftOps(real: ChatOps, ctl: NewChatController): ChatOps {
  return {
    ...real,
    saveDraft: async (_id: string, text: string, revision: number) => {
      ctl.saveText(text, revision);
      return { revision };
    },
    setModel: async (_id: string, model?: string, effort?: string) => ctl.setModel(model, effort),
    attach: async (_id: string, name: string, mime: string, bytes: string) => ({ attachmentId: ctl.addImage({ name, mime, bytes }) }),
    skills: async () => [],
    autocomplete: async () => ({ completion: '', retryAfterMs: null }),
    send: (_id: string, submissionId: string, text: string, action: SendAction, _rev: number, extras: SendExtras = {}) => ctl.firstSend(submissionId, text, action, extras),
    stop: async () => ({ status: 'idle' }),
  };
}
