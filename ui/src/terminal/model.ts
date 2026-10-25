// The terminal dock's state and rules, kept pure for node:test.

export const HEIGHT_DEFAULT = 300;
export const HEIGHT_MIN = 120;
/** The dock leaves at least this much of the main pane above it. */
export const PANE_KEEP = 140;
export const SESSIONS_MAX = 16;
/** Ack after this many UTF-16 units, or on the next frame, whichever comes first. */
export const ACK_EVERY = 64 * 1024;

export function clampHeight(h: number, paneHeight: number): number {
  const max = Math.max(HEIGHT_MIN, paneHeight - PANE_KEEP);
  return Math.round(Math.min(max, Math.max(HEIGHT_MIN, h)));
}

/** A tab's label: the folder it is in, then the shell ("helm pwsh"), never the whole path. */
export function tabLabel(title: string, cwd: string): string {
  const folder = cwd.replace(/[\\/]+$/, '').split(/[\\/]/).pop() ?? '';
  const name = /^[a-z]:$/i.test(folder) ? `${folder}\\` : folder;
  return name ? `${name} ${title}` : title;
}

export type TabState = 'idle' | 'unseen' | 'running' | 'exited';

export interface DockTab {
  id: string;
  title: string;
  cwd: string;
  state: TabState;
  exitCode?: number;
  /** Opened by this dock (not reattached after a reload). */
  fresh?: boolean;
}

/**
 * Event ordering after a reattach: output with a sequence number at or below the one the replay
 * covers is already on screen and is dropped.
 */
export class SeqFilter {
  private last = 0;
  reset(seq: number): void {
    this.last = seq;
  }
  accept(seq: number): boolean {
    if (seq <= this.last) return false;
    this.last = seq;
    return true;
  }
}

/** Batches acknowledgements so a flood is acked in a few large calls, never one per chunk. */
export class AckBatcher {
  private pending = 0;
  private readonly send: (units: number) => void;
  constructor(send: (units: number) => void) {
    this.send = send;
  }
  /** Count units written; returns true when a flush happened. */
  wrote(units: number): boolean {
    this.pending += units;
    if (this.pending >= ACK_EVERY) {
      this.flush();
      return true;
    }
    return false;
  }
  flush(): void {
    if (this.pending > 0) {
      const n = this.pending;
      this.pending = 0;
      this.send(n);
    }
  }
}

/** The tab's dot after an event. The active, visible tab never goes unseen. */
export function nextTabState(prev: TabState, ev: 'output' | 'exit' | 'seen', visible: boolean): TabState {
  if (prev === 'exited') return 'exited';
  if (ev === 'exit') return 'exited';
  if (ev === 'seen') return 'idle';
  return visible ? prev : 'unseen';
}

/**
 * Keys the terminal keeps for itself vs the app. Ctrl+C and every plain Ctrl
 * chord go to the shell; the dock's own commands are Ctrl+Shift chords; the dock toggles (Ctrl+`
 * and Ctrl+J) still reach the app. Returns the dock command, "app" or "shell".
 */
export function routeKey(e: { key: string; code?: string; ctrlKey: boolean; shiftKey: boolean; altKey: boolean; metaKey: boolean }):
  | 'shell'
  | 'app'
  | 'find'
  | 'new'
  | 'close'
  | 'copy'
  | 'paste'
  | 'clear'
  | `select:${number}` {
  const code = e.code ?? '';
  if (e.ctrlKey && !e.altKey && (code === 'Backquote' || (!e.shiftKey && code === 'KeyJ'))) return 'app';
  if (e.ctrlKey && e.shiftKey && !e.altKey) {
    if (code === 'KeyF') return 'find';
    if (code === 'KeyT') return 'new';
    if (code === 'KeyW') return 'close';
    if (code === 'KeyC') return 'copy';
    if (code === 'KeyV') return 'paste';
    if (code === 'KeyK') return 'clear';
    const d = /^Digit([1-9])$/.exec(code);
    if (d) return `select:${Number(d[1])}`;
    return 'app';
  }
  if (e.ctrlKey && e.key === ',') return 'app';
  return 'shell';
}

/**
 * The ANSI palettes. Light is the default theme's; dark is Helm's own pick for the dark theme.
 */
export const PALETTE = {
  light: {
    normal: ['#262826', '#b54a44', '#4f7a3d', '#94701f', '#3c6a9c', '#8a559f', '#2f7f79', '#8c8f88'],
    bright: ['#646762', '#cc5f58', '#5f8f4b', '#ad8530', '#4d7fb3', '#a06bb5', '#3e9690', '#b4b7af'],
  },
  dark: {
    normal: ['#1b1b1b', '#f0908c', '#9fcf87', '#e0c27c', '#8db4e2', '#c9a3dc', '#7fcfc6', '#cbcbcb'],
    bright: ['#5f5f5f', '#ffb0ab', '#b8e6a2', '#f2d89a', '#aecdf3', '#ddc0ec', '#a3e3db', '#ffffff'],
  },
} as const;

/**
 * Remove terminal queries (device status, cursor position, device attributes) from replayed
 * output, so a reattached view does not answer them a second time.
 */
export function stripQueries(text: string): string {
  const ESC = String.fromCharCode(27);
  return text.replace(new RegExp(`${ESC}\\[(?:[56]n|\\?6n|[>=]?c|0?c)`, 'g'), '');
}
