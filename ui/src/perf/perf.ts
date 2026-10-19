// The app's one perf recorder and `window.__helmPerf`. Browser glue for marks.ts; the store and
// the pacer report into it, and the frame-stats overlay reads it.

import { motion, type MotionPolicy } from './cadence.ts';
import { PerfRecorder } from './marks.ts';

const hasWindow = typeof window !== 'undefined' && typeof requestAnimationFrame === 'function';

/** After the next frame is painted: a frame callback, then a task once that frame has gone out. */
function afterPaint(fn: () => void): void {
  if (!hasWindow) return;
  requestAnimationFrame(() => setTimeout(fn, 0));
}

export const perf = new PerfRecorder(() => (typeof performance !== 'undefined' ? performance.now() : Date.now()), afterPaint);

export interface HelmPerf {
  recorder: PerfRecorder;
  /** Low-refresh motion: `motion.low`, and `motion.force('low' | 'normal' | 'auto')` for checks. */
  motion: MotionPolicy;
  /** Shows or hides the frame-stats overlay (also Mod+Shift+P). */
  toggleOverlay: () => void;
}

const global: HelmPerf = { recorder: perf, motion, toggleOverlay: () => undefined };
if (hasWindow) (window as unknown as { __helmPerf?: HelmPerf }).__helmPerf = global;

export function setOverlayToggle(fn: () => void): () => void {
  global.toggleOverlay = fn;
  return () => {
    if (global.toggleOverlay === fn) global.toggleOverlay = () => undefined;
  };
}

/**
 * Notes Send presses: a click on the Send button, or Enter in the composer (with any modifier; a
 * plain Enter that only adds a line is superseded by the real send, or ages out). Capture phase,
 * so the composer cannot hide it. Returns the uninstaller.
 */
export function watchSends(doc: Document): () => void {
  const onClick = (e: Event) => {
    if ((e.target as Element | null)?.closest?.('[data-testid="send"]')) perf.sendIntent();
  };
  const onKey = (e: Event) => {
    const k = e as KeyboardEvent;
    // Shift+Enter only adds a line; Ctrl+Shift+Enter (steer) is a send.
    if (k.key !== 'Enter' || k.isComposing || (k.shiftKey && !k.ctrlKey && !k.metaKey)) return;
    if ((k.target as Element | null)?.closest?.('[data-testid="composer"]')) perf.sendIntent();
  };
  doc.addEventListener('click', onClick, true);
  doc.addEventListener('keydown', onKey, true);
  return () => {
    doc.removeEventListener('click', onClick, true);
    doc.removeEventListener('keydown', onKey, true);
  };
}
