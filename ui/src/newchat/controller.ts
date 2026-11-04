// The new-chat page's state: one draft, held here and not on
// the host. Nothing is created until the first send, so leaving an untouched draft leaves nothing
// behind. Plain TypeScript, tested under node:test; `NewChatHost` connects it to the app.
//
// The draft is a stand-in chat record for the ordinary composer. Its ops (`draftOps`) keep the text,
// model, effort and pasted images here; the first send creates the real chat, moves the images
// onto it, sends, and opens it.

import { ApiError, type Api } from '../api/client.ts';
import type { ChatOps, SendExtras } from '../api/ops.ts';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import type { ChatRecord, SendAction, Submission } from '../api/types.ts';
import { appendBlock, withItemContext } from '../tasks/context.ts';
import { discardUntouched } from './api.ts';
import { NO_DEFAULT_BOARD, type DefaultBoardState } from './boards.ts';
import { boardFor, folderProjectFor, isOpenBoard, needsFolder, pushRecent, readRecents, resolveFolder, samePath, type ResolvedFolder } from './places.ts';

export interface NewChatOptions {
  /** Preselect this board (a folder project stands for the one board whose folder it is). */
  projectId?: string;
  /** Start in this folder. */
  folder?: string;
  /** Text to add to the draft (after anything already typed); never sent by itself. */
  text?: string;
  /** The task-board item this chat is for: the chat is linked to it once its first message is sent. */
  itemId?: string;
  /** The item's title, kept with the link so the sidebar can name it. */
  itemTitle?: string;
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
  /** The board the chat will be shown on; null follows the folder (the board over it, else the
   *  default board). */
  projectId: string | null;
  /** The folder the user picked; null follows the project (folder resolution). */
  folder: string | null;
  text: string;
  revision: number;
  model: string | null;
  effort: string | null;
  images: Map<string, LocalImage>;
  createdAt: number;
  /** The board item this chat is for (see `NewChatOptions.itemId`). */
  item?: { id: string; title?: string };
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
/** The typed text of the open draft, so it survives a restart (images and the place do not). */
export const DRAFT_TEXT_KEY = 'helm.newchat.draftText';

/** The dialogs the new-chat area draws (New project, Add folder, Primary folder…). */
export type NewChatDialog =
  | { kind: 'new-project'; /** The title to start with. */ title?: string; /** Settings opens it without starting a chat after. */ noChat?: boolean }
  | { kind: 'add-project'; /** Open on the New folder tab with this name filled in. */ newName?: string; /** Start a chat in the folder once it is added. */ startChat?: boolean }
  | { kind: 'primary-folder'; projectId: string };

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
  /**
   * A chat the first send created but could not send to, with the place it was made for and the
   * images already uploaded to it. The next try in the same place reuses it; any other outcome
   * asks the host to discard it, which it does only while the chat is still untouched.
   */
  private made: { chat: ChatRecord; key: string; uploads: Map<string, string> } | null = null;

  open = false;
  dialog: NewChatDialog | null = null;
  draft: Draft | null = null;
  projects: ProjectNode[] = [];
  /** The default board and the boards pickers leave out. */
  boards: DefaultBoardState = NO_DEFAULT_BOARD;
  /** Whether a task board is connected. Without one a new project cannot be made there, so
   *  "New project" adds a folder instead. */
  boardConnected = false;
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
      // Opening any chat leaves the page (the chat the first send just made is left alone).
      if (now !== null && this.open) this.leave(now);
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

  openDialog(d: NewChatDialog): void {
    this.dialog = d;
    this.changed();
  }

  closeDialog(): void {
    if (!this.dialog) return;
    this.dialog = null;
    this.changed();
  }

  setProjects(list: ProjectNode[]): void {
    this.projects = list;
    this.changed();
  }

  setBoards(s: DefaultBoardState): void {
    this.boards = s;
    this.changed();
  }

  setBoardConnected(on: boolean): void {
    if (this.boardConnected === on) return;
    this.boardConnected = on;
    this.changed();
  }

  /** "New project" (Ctrl+O, the empty state, the picker's row, Settings): the New project form when
   *  a task board is connected; without one, the Add folder dialog, which then starts a chat in the
   *  new folder (unless `noChat`). */
  newProject(opts: { title?: string; noChat?: boolean } = {}): void {
    const title = opts.title?.trim() || undefined;
    if (this.boardConnected) {
      this.openDialog({ kind: 'new-project', ...(title ? { title } : {}), ...(opts.noChat ? { noChat: true } : {}) });
    } else {
      this.openDialog({ kind: 'add-project', ...(title ? { newName: title } : {}), ...(opts.noChat ? {} : { startChat: true }) });
    }
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
    // The chat the user came from: its board, as far as the project list can tell (its folder's
    // board), then the board it is really shown on once the host answers.
    const fromBoard = place ? boardFor(place.projectId, this.projects) : null;
    let follow = false;
    if (!this.draft) {
      this.draft = this.newDraft(env, explicit ? null : place ? fromBoard : this.lastProject(env));
      follow = !explicit && !!place;
    } else if (!explicit && place && this.untouched()) {
      // An untouched draft is reused, and follows the chat the user came from.
      this.draft.projectId = fromBoard;
      this.draft.folder = null;
      follow = true;
    }
    if (follow && from) this.followBoardOf(env, from, this.draft);
    if (explicit) this.move(opts.projectId !== undefined ? (boardFor(opts.projectId, this.projects) ?? opts.projectId) : this.projectForFolder(opts.folder), opts.folder ?? null, false);
    if (opts.itemId) {
      const title = opts.itemTitle ?? (this.draft.item?.id === opts.itemId ? this.draft.item.title : undefined);
      this.draft.item = { id: opts.itemId, ...(title ? { title } : {}) };
    }
    // Task details go in once: a draft that already holds the item's id is left as it is.
    if (opts.text && this.draft) {
      const next = opts.itemId ? withItemContext(this.draft.text, opts.itemId, opts.text) : appendBlock(this.draft.text, opts.text);
      if (next !== this.draft.text) {
        this.draft.text = next;
        this.draft.revision += 1;
        try {
          env.storage.setItem(DRAFT_TEXT_KEY, next);
        } catch {
          // Storage full or off: the draft still lives in memory.
        }
      }
    }
    this.open = true;
    this.lastSelected = null;
    if (from) env.select(null);
    this.changed();
    this.announce();
  }

  /**
   * Close the page. An untouched draft is discarded; a typed one waits for the next New chat. Pasted
   * images go either way (the composer forgets them when it closes), and so does a chat made by a
   * refused first send, if it is still empty.
   */
  leave(opening: string | null = null): void {
    if (!this.open) return;
    this.open = false;
    this.draft?.images.clear();
    if (this.made && this.made.chat.id === opening) this.made = null;
    this.dropMade();
    if (this.untouched()) this.draft = null;
    this.changed();
  }

  /** Nothing typed and nothing pasted (a model or effort change alone does not count). */
  untouched(): boolean {
    const d = this.draft;
    return !d || (d.text.trim() === '' && d.images.size === 0);
  }

  /** Forget the chat a refused send made, asking the host to discard it if it is still untouched. */
  private dropMade(): void {
    const m = this.made;
    // A send still running owns the chat it made; it clears or keeps it when it settles.
    if (!m || this.sending) return;
    this.made = null;
    if (this.env) void discardUntouched(this.env.api, m.chat.id).catch(() => undefined);
  }

  /** Where the draft would be created: the board and the resolved folder. */
  private placeKey(): string {
    return JSON.stringify([this.board(), this.place().folder]);
  }

  /**
   * Change the draft's board; the folder follows the new board unless `folder` is given. A board
   * with no folder yet asks for one first (the Primary folder dialog).
   */
  move(projectId: string | null, folder: string | null = null, notify = true): void {
    if (!this.draft) return;
    this.draft.projectId = projectId;
    this.draft.folder = folder;
    if (this.made && this.made.key !== this.placeKey()) this.dropMade();
    const p = this.projects.find((x) => x.id === projectId);
    if (p && !folder && needsFolder(p, this.projects)) this.dialog = { kind: 'primary-folder', projectId: p.id };
    if (notify) {
      this.changed();
      this.announce();
    }
  }

  /**
   * The board the chat will be shown on: the one chosen, else the one board over the folder it will
   * run in, else the default board. Null only when there is no board at all (the host then keeps the
   * chat under its folder).
   */
  board(): string | null {
    const chosen = this.draft?.projectId ?? null;
    if (chosen) return boardFor(chosen, this.projects) ?? this.boards.boardId;
    const folder = this.place().folder;
    const over = folder ? this.projects.find((p) => p.kind === 'folder' && !p.archived && !!p.path && samePath(p.path, folder)) : undefined;
    return (over ? boardFor(over.id, this.projects) : null) ?? this.boards.boardId;
  }

  /** Ask the host which board the chat the user came from is shown on, and follow it while the user
   *  has not picked another. */
  private followBoardOf(env: NewChatEnv, chatId: string, d: Draft): void {
    const before = d.projectId;
    void env
      .api<SidebarRow[] | null>('sidebar_rows', { chatIds: [chatId] })
      .then((rows) => {
        const group = rows?.find((r) => r.chatId === chatId)?.groupProjectId;
        if (!group || group === before || this.draft !== d || d.projectId !== before || d.folder !== null) return;
        if (!isOpenBoard(this.projects.find((p) => p.id === group))) return;
        d.projectId = group;
        if (this.made && this.made.key !== this.placeKey()) this.dropMade();
        this.changed();
        this.announce();
      })
      .catch(() => undefined);
  }

  setFolder(folder: string): void {
    if (!this.draft) return;
    this.draft.folder = folder;
    if (this.made && this.made.key !== this.placeKey()) this.dropMade();
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
      localAhead: null,
      started: false,
    };
  }

  // ---- What the composer's ops do on a draft -------------------------------------------------

  saveText(text: string, revision: number): void {
    const d = this.draft;
    if (!d || revision <= d.revision) return;
    const wasUntouched = this.untouched();
    d.text = text;
    d.revision = revision;
    try {
      this.env?.storage.setItem(DRAFT_TEXT_KEY, text);
    } catch {
      // Storage full or off: the draft still lives in memory.
    }
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
    run.then(clear, () => {
      clear();
      // Refused after the page was left: nothing else will ever come back for the chat it made.
      if (!this.open) this.dropMade();
    });
    return run;
  }

  private async doFirstSend(submissionId: string, text: string, action: SendAction, extras: SendExtras): Promise<{ submission: Submission; chat: ChatRecord }> {
    const env = this.env;
    const d = this.draft;
    if (!env || !d) throw new ApiError('not_found', 'The new chat was closed.');
    const { folder } = this.place();
    if (!folder) throw new ApiError('needs_folder', 'Choose a folder for this chat first.');
    const key = this.placeKey();
    if (this.made && this.made.key !== key) this.dropMade();
    let made = this.made;
    if (!made) {
      // The exact folder project when the chosen one is a folder project over this folder
      // (several projects may share a folder); otherwise the host finds or makes the folder's own.
      // The board goes with it, and the host shows the chat there.
      const projectId = folderProjectFor(d.projectId, folder, this.projects);
      const boardId = this.board();
      const chat = await env.api<ChatRecord>('chat_create', {
        cwd: folder,
        ...(projectId ? { projectId } : {}),
        ...(boardId ? { boardId } : {}),
        ...(d.model ? { model: d.model } : {}),
        ...(d.effort ? { effort: d.effort } : {}),
      });
      made = { chat, key, uploads: new Map() };
      this.made = made;
      env.storeChat(chat);
    }
    const chat = made.chat;
    const ids: string[] = [];
    for (const id of extras.attachments ?? []) {
      const img = d.images.get(id);
      if (!img) {
        ids.push(id);
        continue;
      }
      // Uploaded once per made chat: a retry after a refused send sends the same images again.
      let up = made.uploads.get(id);
      if (!up) {
        up = (await env.ops.attach(chat.id, img.name, img.mime, img.bytes)).attachmentId;
        made.uploads.set(id, up);
      }
      ids.push(up);
    }
    const result = await env.ops.send(chat.id, submissionId, text, action, 0, { ...extras, attachments: ids });
    const shownOn = this.board();
    if (shownOn) env.storage.setItem(LAST_PROJECT_KEY, shownOn);
    pushRecent(env.storage, folder);
    // A chat made for a board item is linked to it here, in Helm only. A failure here is not the
    // send's failure: the chat exists and was sent.
    if (d.item) await env.api('item_link_add', { itemId: d.item.id, chatId: chat.id, ...(d.item.title ? { title: d.item.title } : {}) }).catch(() => undefined);
    this.draft = null;
    this.made = null;
    this.open = false;
    try {
      env.storage.setItem(DRAFT_TEXT_KEY, '');
    } catch {
      // As above.
    }
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
    const d = this.makeDraft(env, projectId);
    // The chat is made on Settings > Runtime's default model, so the chip should read it too.
    void env
      .api<{ model?: string | null } | null>('runtime_settings_get', {})
      .then((r) => {
        if (r?.model && this.draft === d && d.model === null) {
          d.model = r.model;
          this.changed();
        }
      })
      .catch(() => undefined);
    return d;
  }

  private makeDraft(env: NewChatEnv, projectId: string | null): Draft {
    return { id: `${DRAFT_PREFIX}${env.uid()}`, projectId, folder: null, text: this.savedText(env), revision: 0, model: null, effort: null, images: new Map(), createdAt: env.now() };
  }

  private savedText(env: NewChatEnv): string {
    try {
      return env.storage.getItem(DRAFT_TEXT_KEY) ?? '';
    } catch {
      return '';
    }
  }

  private lastProject(env: NewChatEnv): string | null {
    const id = env.storage.getItem(LAST_PROJECT_KEY);
    return id && isOpenBoard(this.projects.find((p) => p.id === id)) ? id : null;
  }

  /** The board over `folder`: the one board whose folder it is, if there is exactly one. */
  private projectForFolder(folder: string | undefined): string | null {
    if (!folder) return null;
    for (const p of this.projects) {
      if (folderProjectFor(p.id, folder, this.projects) === null) continue;
      const b = boardFor(p.id, this.projects);
      if (b) return b;
    }
    return null;
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
