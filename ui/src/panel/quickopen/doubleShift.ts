// "Tap Shift twice": the pure part. A tap is Shift pressed and let go with no other key or
// modifier in between; two taps close together are a double tap. Typing a capital letter (Shift,
// then a letter) is never a tap, and holding Shift down (key repeat) counts once.

import { useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';

/** Two taps this close together (ms) make a double tap. */
export const DOUBLE_SHIFT_MS = 400;

export interface KeyInfo {
  key: string;
  repeat?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
}

export class DoubleShift {
  /** Shift went down with nothing else held and nothing pressed since. */
  private clean = false;
  private lastTap = 0;

  down(e: KeyInfo): void {
    if (e.key !== 'Shift') {
      this.clean = false;
      this.lastTap = 0;
      return;
    }
    if (e.repeat) return;
    this.clean = !e.ctrlKey && !e.altKey && !e.metaKey;
    if (!this.clean) this.lastTap = 0;
  }

  /** True when this release completes a double tap. */
  up(e: KeyInfo, now: number): boolean {
    if (e.key !== 'Shift') return false;
    const tapped = this.clean;
    this.clean = false;
    if (!tapped) return false;
    if (this.lastTap > 0 && now - this.lastTap <= DOUBLE_SHIFT_MS) {
      this.lastTap = 0;
      return true;
    }
    this.lastTap = now;
    return false;
  }

  /** The window lost focus or a dialog took over: start again. */
  reset(): void {
    this.clean = false;
    this.lastTap = 0;
  }
}

/** The stored value as the setting: on for anything but an explicit `false`. */
export function parseDoubleShift(v: unknown): boolean {
  return v !== false;
}

/** `doubleShift`, live: re-read when the preference changes. */
export function useDoubleShift(): boolean {
  const shell = useShell();
  const [on, setOn] = useState(true);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'doubleShift' })
      .then((v) => setOn(parseDoubleShift(v)))
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const c = (e as { changed?: Record<string, unknown> }).changed ?? {};
      if ('doubleShift' in c) setOn(parseDoubleShift(c.doubleShift));
    });
  }, [shell]);
  return on;
}
