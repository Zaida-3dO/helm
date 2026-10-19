// Send keys. Pure: the composer passes a key event's shape and the current settings,
// and gets back what to do.
//
// Windows defaults: Mod is Ctrl. A Send now default of Ctrl+Enter would be the same chord on Windows
// as the default send key Mod+Enter, and neither would yield by the chord rule while both are on
// defaults, so Send now is Ctrl+Alt+Enter. Steer is Ctrl+Shift+Enter and Queue is Alt+Enter.

import type { SendAction, SendMode } from '../api/types.ts';

export type SendShortcut = 'mod-enter' | 'enter';

export interface KeyShape {
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
}

export interface KeyContext {
  sendShortcut: SendShortcut;
  mode: SendMode;
  showChoices: boolean;
}

export type KeyResult =
  | { kind: 'send'; action: SendAction }
  | { kind: 'stop-continue' }
  | { kind: 'move'; delta: -1 | 1 }
  | { kind: 'none' };

/** The alternate send actions that are live in a mode. */
export function alternates(mode: SendMode): SendAction[] {
  switch (mode) {
    case 'queue':
      return ['steer'];
    case 'asap':
      return ['queue', 'now'];
    case 'interrupt':
      return ['steer', 'queue'];
  }
}

function chord(k: KeyShape): string {
  const mods = [k.ctrl || k.meta ? 'Mod' : '', k.alt ? 'Alt' : '', k.shift ? 'Shift' : ''].filter(Boolean);
  return [...mods, k.key].join('+');
}

export const CHORDS = {
  send: 'Mod+Enter',
  steer: 'Mod+Shift+Enter',
  queue: 'Alt+Enter',
  now: 'Mod+Alt+Enter',
} as const;

/** Human label for a chord on Windows. */
export function chordLabel(c: string): string {
  return c.replace('Mod', 'Ctrl');
}

export function resolveKey(k: KeyShape, ctx: KeyContext): KeyResult {
  const c = chord(k);
  if (c === 'Escape') return { kind: 'stop-continue' };
  if (k.key !== 'Enter') return { kind: 'none' };

  const live = ctx.showChoices ? alternates(ctx.mode) : [];
  const alt = (a: SendAction): KeyResult => ({ kind: 'send', action: live.includes(a) ? a : 'default' });

  if (c === CHORDS.steer) return alt('steer');
  if (c === CHORDS.queue) return alt('queue');
  if (c === CHORDS.now) return alt('now');
  if (c === CHORDS.send) return { kind: 'send', action: 'default' };
  if (ctx.sendShortcut === 'enter' && c === 'Enter') return { kind: 'send', action: 'default' };
  return { kind: 'none' };
}

/** What Send will do now, for the send button's tooltip. */
export function sendTooltip(ctx: KeyContext, busy: boolean): string {
  const key = ctx.sendShortcut === 'enter' ? 'Enter' : chordLabel(CHORDS.send);
  if (!busy) return `Send · ${key}`;
  const what = ctx.mode === 'asap' ? 'Steer into the running task' : ctx.mode === 'queue' ? 'Queue message' : 'Interrupt and send';
  const live = ctx.showChoices ? alternates(ctx.mode) : [];
  const hint = live[0] ? ` · ${chordLabel(CHORDS[live[0] === 'default' ? 'send' : live[0]])} to ${verb(live[0])}` : '';
  return `${what} · ${key}${hint}`;
}

function verb(a: SendAction): string {
  return a === 'steer' ? 'steer' : a === 'queue' ? 'queue' : a === 'now' ? 'send now' : 'send';
}

/** The Stop button's tooltip. */
export function stopTooltip(stopSendsWaiting: boolean, showChoices: boolean): string {
  const base = stopSendsWaiting ? 'Stop task and send waiting messages' : 'Stop task and pause queue';
  return showChoices ? `${base} · Esc interrupts and continues the queue` : base;
}

/** What Send does now, for its accessible name (the button is an icon). */
export function sendLabel(busy: boolean, mode: string): string {
  if (!busy) return 'Send';
  return mode === 'asap' ? 'Steer' : mode === 'queue' ? 'Queue' : 'Interrupt';
}
