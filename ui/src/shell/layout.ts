// The sidebar's width and visibility. One small store, read by
// the frame (App.tsx), the top bar (its Show sidebar button) and Ctrl+B; written by the resizer and
// the toggle. Both persist through the generic `preference` op (`sidebarWidth`, `sidebarVisible`).
//
// The stored values are what the user chose; what shows is fitted to the room the frame has (the
// window less the side panel): the sidebar narrows so the chat keeps MAIN_KEEP, and under HIDE_BELOW
// it hides behind its toggle. Neither fitting is saved.

export const SIDEBAR_DEFAULT = 480;
export const SIDEBAR_MIN = 200;
/** The main pane never gets narrower than this while the sidebar is dragged. */
export const MAIN_MIN = 320;
/** The chat's width the sidebar gives way to, unless the user drags it wider. */
export const MAIN_KEEP = 560;
/** Below this much room the sidebar hides by itself; its toggle still shows it. */
export const HIDE_BELOW = 900;
export const SIDEBAR_WIDTH_PREF = 'sidebarWidth';
export const SIDEBAR_VISIBLE_PREF = 'sidebarVisible';

/** A stored width, or the default for anything that is not a usable number (the host allows 200-4096). */
export function parseSidebarWidth(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) && v >= SIDEBAR_MIN && v <= 4096 ? Math.round(v) : SIDEBAR_DEFAULT;
}

/** Visible unless the stored value is exactly false. */
export function parseSidebarVisible(v: unknown): boolean {
  return v !== false;
}

/**
 * The width while dragging: the start width moved by the pointer, at least SIDEBAR_MIN, and never
 * so wide that the main pane (measured when the drag began) drops under MAIN_MIN.
 */
export function dragWidth(start: number, dx: number, mainAtStart: number): number {
  const max = Math.max(SIDEBAR_MIN, start + Math.max(0, mainAtStart - MAIN_MIN));
  return Math.round(Math.min(max, Math.max(SIDEBAR_MIN, start + dx)));
}

export interface SidebarLayout {
  width: number;
  visible: boolean;
}

/** What shows, and why it differs from what is stored: `shrunk` to leave the chat room, `hidden` for a narrow window. */
export interface FittedLayout extends SidebarLayout {
  fitted: 'shrunk' | 'hidden' | null;
}

export interface FitState {
  /** The frame's width less the side panel; null until measured. */
  room: number | null;
  /** The toggle showed the sidebar in a window too narrow for it. */
  peek: boolean;
  /** The user dragged the sidebar wider than MAIN_KEEP allows; the chat may then drop to MAIN_MIN. */
  squeeze: boolean;
}

/** The sidebar that shows for a stored layout in `room`. */
export function fitSidebar(stored: SidebarLayout, { room, peek, squeeze }: FitState): FittedLayout {
  if (!stored.visible || room === null) return { ...stored, fitted: null };
  if (room < HIDE_BELOW && !peek) return { width: stored.width, visible: false, fitted: 'hidden' };
  const max = Math.max(SIDEBAR_MIN, room - (squeeze ? MAIN_MIN : MAIN_KEEP));
  const width = Math.min(stored.width, max);
  return { width, visible: true, fitted: width < stored.width ? 'shrunk' : null };
}

type Saver = (key: string, value: unknown) => void;

class LayoutStore {
  private stored: SidebarLayout = { width: SIDEBAR_DEFAULT, visible: true };
  private fit: FitState = { room: null, peek: false, squeeze: false };
  /** The width while a drag is under way. */
  private dragging: number | null = null;
  private shown: FittedLayout = { ...this.stored, fitted: null };
  private readonly listeners = new Set<() => void>();
  private saver: Saver = () => undefined;

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  /** What shows. */
  get = (): FittedLayout => this.shown;

  /** What the user chose. */
  preferred = (): SidebarLayout => this.stored;

  /** Where changes are saved (the shell connects the preference op). */
  connect(save: Saver): void {
    this.saver = save;
  }

  /** Apply stored values without saving them back. */
  load(width: unknown, visible: unknown): void {
    this.update({ width: parseSidebarWidth(width), visible: parseSidebarVisible(visible) });
  }

  /** The room the frame measured. Coming back above HIDE_BELOW ends a peek. */
  setRoom(room: number): void {
    const r = Math.round(room);
    if (r === this.fit.room) return;
    this.update(this.stored, { ...this.fit, room: r, peek: r < HIDE_BELOW && this.fit.peek });
  }

  /** Show a width while dragging; null ends the preview. */
  preview(width: number | null): void {
    this.dragging = width;
    this.update(this.stored);
  }

  /** Set the chosen width. A width wider than the chat's MAIN_KEEP allows is kept by letting the chat narrow. */
  setWidth(width: number, persist: boolean): void {
    const room = this.fit.room;
    const squeeze = room !== null && width > Math.max(SIDEBAR_MIN, room - MAIN_KEEP);
    this.update({ ...this.stored, width }, { ...this.fit, squeeze });
    if (persist) this.saver(SIDEBAR_WIDTH_PREF, width);
  }

  /** Saved, except that hiding in a window too narrow for the sidebar only ends the peek (it shows again in a wide one). */
  setVisible(visible: boolean): void {
    const narrow = this.fit.room !== null && this.fit.room < HIDE_BELOW;
    const fit = narrow ? { ...this.fit, peek: visible } : this.fit;
    if (visible === this.stored.visible || (narrow && !visible)) return this.update(this.stored, fit);
    this.update({ ...this.stored, visible }, fit);
    this.saver(SIDEBAR_VISIBLE_PREF, visible);
  }

  toggle = (): void => this.setVisible(!this.shown.visible);

  private update(stored: SidebarLayout, fit: FitState = this.fit): void {
    this.stored = stored;
    this.fit = fit;
    const f = fitSidebar(stored, fit);
    const next = this.dragging !== null && f.visible ? { ...f, width: this.dragging } : f;
    const s = this.shown;
    if (next.width === s.width && next.visible === s.visible && next.fitted === s.fitted) return;
    this.shown = next;
    for (const l of [...this.listeners]) l();
  }
}

/** The app's one sidebar layout. The sidebar's title-strip Hide button calls `sidebarLayout.toggle()`. */
export const sidebarLayout = new LayoutStore();
