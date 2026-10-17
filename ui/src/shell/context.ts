// The shell context: what every top-level component reaches through
// `useShell()`. One `ShellController` per app holds the selection, the command registry and the
// open dialog; it is plain TypeScript so its rules are tested under node:test.
//
// `selection` is persisted in the `selection` preference and restored at launch.
// Opening a chat from the tray, a toast, the hotkey or a search hit all go through `select`.

import { createContext, createElement, useContext, useSyncExternalStore, type ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ChatOps } from '../api/ops.ts';
import type { ChatStore } from '../state/store.ts';
import { keys } from '../ui/keys.ts';

export interface Selection {
  chatId: string | null;
  projectId: string | null;
}

export interface SelectOptions {
  /** Open the chat scrolled to this message (search hits, saved items). */
  messageId?: string;
  /** The project the selection belongs to, when the caller knows it. */
  projectId?: string | null;
}

export interface ShellCommand {
  title: string;
  /** A chord such as "Ctrl+K" (Mod and Cmd read as Ctrl). */
  keys?: string;
  run: () => void;
}

/** A pending scroll target, read once by the chat view that opens. */
export interface FocusRequest {
  chatId: string;
  messageId: string;
}

export const SELECTION_PREF = 'selection';
const PERSIST_DELAY_MS = 300;

export const EMPTY_SELECTION: Selection = { chatId: null, projectId: null };

/** Parse a stored selection; anything malformed reads as nothing selected. */
export function parseSelection(v: unknown): Selection {
  if (!v || typeof v !== 'object') return EMPTY_SELECTION;
  const o = v as Record<string, unknown>;
  const s = (x: unknown) => (typeof x === 'string' && x.length > 0 && x.length <= 200 ? x : null);
  return { chatId: s(o.chatId), projectId: s(o.projectId) };
}

type Listener = () => void;

export class ShellController {
  private sel: Selection = EMPTY_SELECTION;
  private dialog: string | null = null;
  private focus: FocusRequest | null = null;
  private readonly listeners = new Set<Listener>();
  private persistTimer: ReturnType<typeof setTimeout> | undefined;
  private restored = false;
  private restoreDone = false;

  readonly api: Api;
  readonly bus: EventBus;
  readonly store: ChatStore;
  readonly ops: ChatOps;

  constructor(api: Api, bus: EventBus, store: ChatStore, ops: ChatOps) {
    this.api = api;
    this.bus = bus;
    this.store = store;
    this.ops = ops;
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  private changed(): void {
    for (const l of [...this.listeners]) l();
  }

  get selection(): Selection {
    return this.sel;
  }

  get openDialogId(): string | null {
    return this.dialog;
  }

  /** Restore the persisted selection once at launch; a selection made meanwhile wins. */
  async restore(): Promise<void> {
    if (this.restored) return;
    this.restored = true;
    try {
      const v = await this.api<unknown>('preference_get', { key: SELECTION_PREF });
      if (this.sel === EMPTY_SELECTION) {
        this.sel = parseSelection(v);
        this.changed();
      }
    } catch {
      // A missing or unreadable preference means nothing selected.
    }
    this.restoreDone = true;
  }

  select = (chatId: string | null, opts: SelectOptions = {}): void => {
    const projectId = opts.projectId !== undefined ? opts.projectId : chatId === this.sel.chatId ? this.sel.projectId : null;
    const next: Selection = { chatId, projectId };
    this.focus = chatId && opts.messageId ? { chatId, messageId: opts.messageId } : null;
    if (next.chatId === this.sel.chatId && next.projectId === this.sel.projectId && !this.focus) return;
    this.sel = next;
    this.changed();
    this.schedulePersist();
  };

  /** Take (and clear) the scroll target for `chatId`, if one is pending. */
  takeFocus(chatId: string): FocusRequest | null {
    if (this.focus?.chatId !== chatId) return null;
    const f = this.focus;
    this.focus = null;
    return f;
  }

  private schedulePersist(): void {
    clearTimeout(this.persistTimer);
    this.persistTimer = setTimeout(() => void this.flush(), PERSIST_DELAY_MS);
  }

  /** Write the selection now (also a quit flusher). */
  async flush(): Promise<void> {
    clearTimeout(this.persistTimer);
    this.persistTimer = undefined;
    // Until the stored selection has been read, writing would replace it with an empty one.
    if (!this.restoreDone && this.sel === EMPTY_SELECTION) return;
    try {
      await this.api('preference', { key: SELECTION_PREF, value: this.sel });
    } catch {
      // Losing the selection on a failed write is harmless; the next change retries.
    }
  }

  /** Register a command in the app's one key registry (ui/keys.ts). */
  registerCommand = (id: string, cmd: ShellCommand): (() => void) =>
    keys.register({ id, title: cmd.title, defaultKeys: cmd.keys ?? null, run: cmd.run });

  openDialog = (id: string): void => {
    if (this.dialog === id) return;
    this.dialog = id;
    this.changed();
  };

  closeDialog = (id?: string): void => {
    if (this.dialog === null || (id !== undefined && this.dialog !== id)) return;
    this.dialog = null;
    this.changed();
  };
}

const ShellReactContext = createContext<ShellController | null>(null);

export function ShellProvider({ shell, children }: { shell: ShellController; children?: ReactNode }) {
  return createElement(ShellReactContext.Provider, { value: shell }, children);
}

/** The shell controller. Throws outside the frame, which is a wiring bug. */
export function useShell(): ShellController {
  const s = useContext(ShellReactContext);
  if (!s) throw new Error('useShell() outside <ShellProvider>');
  return s;
}

/** The current selection, re-rendering on change. */
export function useSelection(): Selection {
  const s = useShell();
  return useSyncExternalStore(s.subscribe, () => s.selection);
}

/** Whether dialog `id` is open, plus a closer. */
export function useDialog(id: string): { open: boolean; close: () => void } {
  const s = useShell();
  const open = useSyncExternalStore(s.subscribe, () => s.openDialogId === id);
  return { open, close: () => s.closeDialog(id) };
}
