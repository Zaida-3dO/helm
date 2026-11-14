// What the top bar's graphics-card control shows and says, from the host's lock view. Pure, so it
// is tested without a window.

import type { LockView } from './api.ts';
import type { TopBarTier } from '../topbar/fold.ts';

/** The control's face: the unload button, the "GPU reserved" pill, or nothing. */
export type GpuFace = 'unload' | 'locked' | 'none';

export interface GpuControlView {
  face: GpuFace;
  /** The button's or pill's visible text; the unload button shows an icon and uses this as its label. */
  label: string;
  /** The hover text. */
  tooltip: string;
  /** `manual` or `game` while locked. */
  lock?: 'manual' | 'game';
  /** An unload or a turn back on is still running: the pill shows the wheel. */
  working: boolean;
  /** A game holds the lock: the popover offers to keep models off after it closes. */
  keepOff?: boolean;
}

/** "21:04", in the user's own clock. */
export function clockTime(ms: number): string {
  const d = new Date(ms);
  const two = (n: number) => String(n).padStart(2, '0');
  return `${two(d.getHours())}:${two(d.getMinutes())}`;
}

/** The plain sentence for why local models are unloaded; it leads the popover and is the pill's hover text. */
export function lockSentence(v: LockView): string {
  if (v.state === 'game') return `Local models are unloaded while ${v.game ?? 'a game'} runs`;
  return v.since !== undefined ? `Local models are unloaded since ${clockTime(v.since)}` : 'Local models are unloaded';
}

/** The sentence under it: when they come back. */
export function comeBackSentence(v: Pick<LockView, 'state' | 'liftMinutes'>): string {
  if (v.state === 'game') {
    const n = v.liftMinutes;
    return `They come back ${n} minute${n === 1 ? '' : 's'} after it closes, or turn them back on here.`;
  }
  return 'They stay off until you turn them back on.';
}

/**
 * What the control shows. The unload button needs a server switched on and is dropped from the
 * narrow and tight bars; the pill is the one thing that must always be seen, so it shows in every tier.
 */
export function gpuControl(view: LockView | null, tier: TopBarTier): GpuControlView {
  if (!view) return { face: 'none', label: '', tooltip: '', working: false };
  if (view.state === 'on') {
    // No `lock`: there is nothing to turn on again, so the pill does not open its popover.
    if (view.working) return { face: 'locked', label: 'Turning local models back on…', tooltip: 'Local models are being turned back on', working: true };
    const show = view.anyLocal && tier !== 'narrow' && tier !== 'tight';
    return { face: show ? 'unload' : 'none', label: 'Unload local models', tooltip: 'Unload local models and free the graphics card', working: false };
  }
  return {
    face: 'locked',
    label: view.working ? 'Unloading…' : 'GPU reserved',
    tooltip: lockSentence(view),
    lock: view.state,
    working: view.working,
    keepOff: view.state === 'game',
  };
}

/** The two lines of the confirm dialog's body. */
export function unloadConfirmLines(restartTask?: string): [string, string] {
  const task = restartTask ? `, the restart task "${restartTask}" is paused` : '';
  return ['This stops all running local-model chats. Unload?', `Strata and Ollama unload their models${task} and nothing loads a model until you turn them back on.`];
}
