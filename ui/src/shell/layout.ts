// The sidebar's width and visibility. One small store, read by
// the frame (App.tsx), the top bar (its Show sidebar button) and Ctrl+B; written by the resizer and
// the toggle. Both persist through the generic `preference` op (`sidebarWidth`, `sidebarVisible`).
//
// The stored values are what the user chose; what shows is fitted to the frame: the sidebar narrows
// so the chat keeps MAIN_KEEP beside the side panel, and in a frame under HIDE_BELOW (a narrow
// window, whatever the panel does) it hides behind its toggle. Neither fitting is saved.

export const SIDEBAR_DEFAULT = 480;
export const SIDEBAR_MIN = 200;
/** The main pane never gets narrower than this while the sidebar is dragged. */
export const MAIN_MIN = 320;
/** The chat's width the sidebar gives way to, unless the user drags it wider. */
export const MAIN_KEEP = 560;
/** In a frame narrower than this the sidebar hides by itself; its toggle still shows it. */
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
  /** The frame's (the window's) width; null until measured. */
  frame: number | null;
  /** What the side panel takes beside the chat: 0 when it is shut or maximised (it then covers the chat, not the sidebar). */
  panel: number;
  /** The toggle showed the sidebar in a window too narrow for it. */
  peek: boolean;
  /** The user dragged the sidebar wider than MAIN_KEEP allows; the chat may then drop to MAIN_MIN. */
  squeeze: boolean;
}

/** The widest the sidebar may show without squeezing the chat under `keep`. */
function widest(frame: number, panel: number, keep: number): number {
  return Math.max(SIDEBAR_MIN, frame - panel - keep);
}

/** The sidebar that shows for a stored layout in a frame. */
export function fitSidebar(stored: SidebarLayout, { frame, panel, peek, squeeze }: FitState): FittedLayout {
  if (!stored.visible || frame === null) return { ...stored, fitted: null };
  if (frame < HIDE_BELOW && !peek) return { width: stored.width, visible: false, fitted: 'hidden' };
  const max = widest(frame, panel, squeeze ? MAIN_MIN : MAIN_KEEP);
  const width = Math.min(stored.width, max);
  return { width, visible: true, fitted: width < stored.width ? 'shrunk' : null };
}

type Saver = (key: string, value: unknown) => void;

class LayoutStore {
  private stored: SidebarLayout = { width: SIDEBAR_DEFAULT, visible: true };
  private fit: FitState = { frame: null, panel: 0, peek: false, squeeze: false };
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

  /** Apply stored values without saving them back. Only a drag stores a width over the default,
   *  so such a width is a drag's and is kept as one (the chat may narrow for it). */
  load(width: unknown, visible: unknown): void {
    const w = parseSidebarWidth(width);
    this.update({ width: w, visible: parseSidebarVisible(visible) }, { ...this.fit, squeeze: w > SIDEBAR_DEFAULT });
  }

  /** The frame's width and what the side panel takes beside the chat. Coming back above HIDE_BELOW ends a peek. */
  setRoom(frame: number, panel = 0): void {
    const f = Math.round(frame);
    const p = Math.round(panel);
    if (f === this.fit.frame && p === this.fit.panel) return;
    this.update(this.stored, { ...this.fit, frame: f, panel: p, peek: f < HIDE_BELOW && this.fit.peek });
  }

  /** Show a width while dragging; null ends the preview. */
  preview(width: number | null): void {
    this.dragging = width;
    this.update(this.stored);
  }

  /** Set the chosen width. A width wider than the chat's MAIN_KEEP allows is kept by letting the chat narrow. */
  setWidth(width: number, persist: boolean): void {
    const { frame, panel } = this.fit;
    const squeeze = frame !== null && width > widest(frame, panel, MAIN_KEEP);
    this.update({ ...this.stored, width }, { ...this.fit, squeeze });
    if (persist) this.saver(SIDEBAR_WIDTH_PREF, width);
  }

  /** Saved, except that hiding in a window too narrow for the sidebar only ends the peek (it shows again in a wide one). */
  setVisible(visible: boolean): void {
    const narrow = this.fit.frame !== null && this.fit.frame < HIDE_BELOW;
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
