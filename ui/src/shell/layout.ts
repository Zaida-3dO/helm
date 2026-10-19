// The sidebar's width and visibility. One small store, read by
// the frame (App.tsx), the top bar (its Show sidebar button) and Ctrl+B; written by the resizer and
// the toggle. Both persist through the generic `preference` op (`sidebarWidth`, `sidebarVisible`).

export const SIDEBAR_DEFAULT = 480;
export const SIDEBAR_MIN = 200;
/** The main pane never gets narrower than this while the sidebar is dragged. */
export const MAIN_MIN = 320;
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

type Saver = (key: string, value: unknown) => void;

class LayoutStore {
  private state: SidebarLayout = { width: SIDEBAR_DEFAULT, visible: true };
  private readonly listeners = new Set<() => void>();
  private saver: Saver = () => undefined;

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get = (): SidebarLayout => this.state;

  /** Where changes are saved (the shell connects the preference op). */
  connect(save: Saver): void {
    this.saver = save;
  }

  /** Apply stored values without saving them back. */
  load(width: unknown, visible: unknown): void {
    this.set({ width: parseSidebarWidth(width), visible: parseSidebarVisible(visible) });
  }

  setWidth(width: number, persist: boolean): void {
    this.set({ ...this.state, width });
    if (persist) this.saver(SIDEBAR_WIDTH_PREF, width);
  }

  setVisible(visible: boolean): void {
    if (visible === this.state.visible) return;
    this.set({ ...this.state, visible });
    this.saver(SIDEBAR_VISIBLE_PREF, visible);
  }

  toggle = (): void => this.setVisible(!this.state.visible);

  private set(next: SidebarLayout): void {
    if (next.width === this.state.width && next.visible === this.state.visible) return;
    this.state = next;
    for (const l of [...this.listeners]) l();
  }
}

/** The app's one sidebar layout. The sidebar's title-strip Hide button calls `sidebarLayout.toggle()`. */
export const sidebarLayout = new LayoutStore();
