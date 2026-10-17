// The panel registry: kinds, slots and the one panel host every area opens tabs through.
//
// `registerPanelKind` adds a kind. `openInPanel` returns false when the
// open is refused (unknown kind, or no room under the caps); the caller then falls back (the modal
// viewer, the external browser). The store is a plain object with `subscribe`/`version`, read by
// React through `useSyncExternalStore`.

import type { ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import { PanelModel } from './model.ts';
import type { CloseReason, Intent, KindRules, PanelTab, SlotId } from './model.ts';

export interface TabContext {
  /** This tab is the strip's selected entry. */
  active: boolean;
  maximized: boolean;
  /** Active, panel shown and documents not hidden: kinds gate their reads on this. */
  visible: boolean;
  /** Visible and the window has focus: kinds gate polling on this. */
  focused: boolean;
  intent: Intent;
  setTitle: (title: string) => void;
  setData: (data: unknown) => void;
  markUserIntent: () => void;
}

export interface PanelKind {
  kind: string;
  title: string | ((tab: Readonly<PanelTab>) => string);
  icon?: (tab: Readonly<PanelTab>) => ReactNode;
  render: (tab: Readonly<PanelTab>, ctx: TabContext) => ReactNode;
  defaultMaximized?: boolean | ((data: unknown) => boolean);
  keepMounted?: boolean;
  headerActions?: (tab: Readonly<PanelTab>, ctx: TabContext) => ReactNode;
  menuEntry?: { label: string; icon?: ReactNode; enabled: () => boolean; open: () => void };
  scope?: 'chat' | 'app';
  preferredWidth?: number;
  maxTabs?: number;
  evictable?: boolean;
  onClose?: (tab: Readonly<PanelTab>, reason: CloseReason) => void;
  persist?: {
    version: number;
    save: (tab: Readonly<PanelTab>) => unknown;
    restore: (data: unknown, version: number) => unknown;
  };
}

export interface PanelSlot {
  title: string;
  icon?: ReactNode;
  render: (ctx: { chatId: string | null; visible: boolean }) => ReactNode;
}

type Listener = () => void;

class PanelStore {
  readonly kinds = new Map<string, PanelKind>();
  readonly slots = new Map<SlotId, PanelSlot>();
  readonly model = new PanelModel((kind) => this.rules(kind));
  private readonly listeners = new Set<Listener>();
  /** The chat being shown; set by the side panel from the app's selection. */
  current: string | null = null;
  /** Bumped on every change; React reads it as the snapshot. */
  version = 0;
  width = 370;
  notice: { id: number; text: string } | null = null;
  private noticeSeq = 0;
  /** The host bridge, set by the side panel when it mounts; kinds read it to make their ops. */
  api: Api | null = null;
  /** The modal viewer: the fallback when a file cannot open as a tab. */
  modal: { path: string; line?: number; column?: number; chatId: string | null; nonce: number } | null = null;
  private modalSeq = 0;

  openModal(path: string, opts: { line?: number; column?: number; chatId: string | null }): void {
    this.modal = { path, ...opts, nonce: ++this.modalSeq };
    this.changed();
  }

  closeModal(): void {
    this.modal = null;
    this.changed();
  }

  private rules(kind: string): KindRules | undefined {
    const k = this.kinds.get(kind);
    if (!k) return undefined;
    return {
      scope: k.scope ?? 'chat',
      maxTabs: k.maxTabs,
      evictable: k.evictable,
      defaultMaximized: k.defaultMaximized,
      onClose: k.onClose,
    };
  }

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  getVersion = (): number => this.version;

  changed(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  /** Runs a model mutation and notifies once. */
  update<T>(fn: (m: PanelModel) => T): T {
    const out = fn(this.model);
    this.changed();
    return out;
  }

  setCurrent(chatId: string | null): void {
    if (this.current === chatId) return;
    this.current = chatId;
    this.changed();
  }

  showNotice(text: string): void {
    this.notice = { id: ++this.noticeSeq, text };
    this.changed();
  }

  clearNotice(id: number): void {
    if (this.notice?.id !== id) return;
    this.notice = null;
    this.changed();
  }
}

export const panelStore = new PanelStore();

/** Registers (or replaces) a kind. Returns a function that unregisters it. */
export function registerPanelKind(kind: PanelKind): () => void {
  panelStore.kinds.set(kind.kind, kind);
  panelStore.changed();
  return () => {
    if (panelStore.kinds.get(kind.kind) === kind) {
      panelStore.kinds.delete(kind.kind);
      panelStore.changed();
    }
  };
}

/** Fills one of the Inspector's fixed slots (Changes, Files). */
export function registerPanelSlot(slot: SlotId, def: PanelSlot): () => void {
  panelStore.slots.set(slot, def);
  panelStore.changed();
  return () => {
    if (panelStore.slots.get(slot) === def) {
      panelStore.slots.delete(slot);
      panelStore.changed();
    }
  };
}

/**
 * Opens a document in the side panel. `false` means refused: nothing changed, so fall back.
 * A user open shows the panel and selects the tab; a program open only adds or updates it.
 */
export function openInPanel(
  kind: string,
  key: string,
  data: unknown,
  opts: { intent: Intent; chatId?: string | null },
): boolean {
  const tab = panelStore.update((m) => m.open(kind, key, data, opts, panelStore.current));
  return tab !== null;
}

/** Shows the Inspector with one of its slots selected. */
export function showSlot(slot: SlotId): void {
  panelStore.update((m) => m.setInspector(true, slot, panelStore.current));
}

/** The top bar's panel toggle and Mod+Shift+I. */
export function togglePanel(): void {
  panelStore.update((m) => m.toggle(panelStore.current));
}

export function isPanelVisible(): boolean {
  return panelStore.model.visible(panelStore.current);
}

// Dev builds only (vite dev, never the bundled app): a console handle for checking the panel by hand.
if (import.meta.env?.DEV && typeof window !== 'undefined') {
  (window as unknown as Record<string, unknown>).__helmPanel = { panelStore, registerPanelKind, openInPanel, showSlot };
}

export function kindTitle(tab: Readonly<PanelTab>): string {
  if (tab.title) return tab.title;
  const k = panelStore.kinds.get(tab.kind);
  if (!k) return tab.key;
  return typeof k.title === 'function' ? k.title(tab) : k.title;
}
