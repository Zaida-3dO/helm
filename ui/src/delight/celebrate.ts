// Small celebrations. When a chat finishes, a thin ring in the accent colour sweeps once around its
// row in the sidebar (Maximal adds a little spray of dots); when a pull request merges, the same
// ring goes around the merge box and a check pops. Quiet, reduced motion, the slow-display mode and
// an unfocused window all mean nothing is drawn.
//
// The decisions (may it run, how closely may celebrations follow each other, is one too old) live in
// `createCelebrator`, which takes everything it touches as parameters so it can be tested; the
// drawing is plain DOM in one fixed, click-through layer.

import { flair, mayMove, type Flair } from '../shell/motion.ts';

/** At most one celebration starts in this many milliseconds; the rest wait their turn. */
export const MIN_GAP_MS = 180;
/** A celebration that would start later than this after its completion is dropped. */
export const MAX_AGE_MS = 1200;
export const RING_MS = 700;
export const FADE_MS = 200;
export const SPRAY_DOTS = 10;

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** What to draw: a ring around `rect`, with optional spray and check. */
export interface Effect {
  rect: Rect;
  /** The ring's corner radius in px. */
  radius: number;
  spray: boolean;
  check: boolean;
}

export interface CompletionLike {
  chatId: string;
  /** When the reply finished (Unix ms), if the event says. */
  completedAt?: number;
}

export interface Deps {
  flair(): Flair;
  mayMove(): boolean;
  hasFocus(): boolean;
  now(): number;
  /** The sidebar row for a chat, when it is on screen. */
  rowRect(chatId: string): Rect | null;
  draw(effect: Effect): void;
  later(fn: () => void, ms: number): void;
}

const ROW_RADIUS = 6;
const BOX_RADIUS = 8;

export function createCelebrator(deps: Deps) {
  let nextSlot = -Infinity;
  const allowed = () => deps.flair() !== 'quiet' && deps.mayMove() && deps.hasFocus();
  return {
    /** A reply finished: ring its row, in turn, if the row is showing and the moment has not passed. */
    completion(c: CompletionLike): void {
      if (!allowed() || !deps.rowRect(c.chatId)) return;
      const now = deps.now();
      const born = typeof c.completedAt === 'number' ? c.completedAt : now;
      const at = Math.max(now, nextSlot);
      if (at - born > MAX_AGE_MS) return;
      nextSlot = at + MIN_GAP_MS;
      const run = () => {
        // The window, the level or the list may have changed while this waited.
        const rect = allowed() ? deps.rowRect(c.chatId) : null;
        if (rect) deps.draw({ rect, radius: ROW_RADIUS, spray: deps.flair() === 'maximal', check: false });
      };
      if (at <= now) run();
      else deps.later(run, at - now);
    },
    /** A pull request merged: ring the merge box and pop a check. */
    merged(rect: Rect | null): void {
      if (!rect || !allowed()) return;
      deps.draw({ rect, radius: BOX_RADIUS, spray: deps.flair() === 'maximal', check: true });
    },
  };
}

// --- drawing ----------------------------------------------------------------------------------

const LAYER_ID = 'delight-layer';

function layer(doc: Document): HTMLElement {
  let el = doc.getElementById(LAYER_ID);
  if (!el) {
    el = doc.createElement('div');
    el.id = LAYER_ID;
    el.setAttribute('aria-hidden', 'true');
    doc.body.appendChild(el);
  }
  return el;
}

function box(el: HTMLElement, r: Rect): void {
  el.style.left = `${r.left}px`;
  el.style.top = `${r.top}px`;
  el.style.width = `${r.width}px`;
  el.style.height = `${r.height}px`;
}

const CHECK_PATH = 'M5.5 12.5l4.2 4.2L18.5 7.6';

/** Draw one effect into the layer; everything it adds removes itself. */
export function drawEffect(e: Effect, doc: Document = document, random: () => number = Math.random): void {
  const host = layer(doc);
  const add = (el: HTMLElement, lifeMs: number) => {
    host.appendChild(el);
    setTimeout(() => el.remove(), lifeMs);
  };
  const ring = doc.createElement('div');
  ring.className = 'delight-ring';
  box(ring, e.rect);
  ring.style.setProperty('--r', `${e.radius}px`);
  const line = doc.createElement('div');
  line.className = 'delight-ring-line';
  ring.appendChild(line);
  add(ring, RING_MS + FADE_MS + 100);
  if (e.check) {
    const badge = doc.createElement('div');
    badge.className = 'delight-check';
    badge.style.left = `${e.rect.left + e.rect.width - 18}px`;
    badge.style.top = `${e.rect.top - 14}px`;
    badge.innerHTML = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="${CHECK_PATH}"/></svg>`;
    add(badge, 1500);
  }
  if (e.spray) {
    const x = e.rect.left + e.rect.width;
    const y = e.rect.top + e.rect.height / 2;
    for (let i = 0; i < SPRAY_DOTS; i++) {
      const dot = doc.createElement('div');
      const ms = 600 + Math.round(random() * 300);
      dot.className = 'delight-dot';
      dot.style.left = `${x - 2}px`;
      dot.style.top = `${y - 2}px`;
      dot.style.setProperty('--dx', `${20 + Math.round(random() * 40)}px`);
      dot.style.setProperty('--dy', `${Math.round((random() - 0.5) * 64)}px`);
      dot.style.setProperty('--d', `${ms}ms`);
      dot.style.setProperty('--c', i % 2 ? 'color-mix(in srgb, var(--accent) 45%, transparent)' : 'var(--accent)');
      add(dot, ms + 100);
    }
  }
}

const toRect = (r: DOMRect | { left: number; top: number; width: number; height: number }): Rect => ({ left: r.left, top: r.top, width: r.width, height: r.height });

/** The on-screen sidebar row for a chat, or null (not listed, scrolled out, or the sidebar hidden). */
export function visibleRowRect(chatId: string, doc: Document = document): Rect | null {
  for (const row of doc.querySelectorAll<HTMLElement>('[data-testid="chat-item"]')) {
    if (row.dataset.chatId !== chatId) continue;
    const r = row.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    const side = row.closest('aside')?.getBoundingClientRect();
    const middle = r.top + r.height / 2;
    if (side && (middle < side.top || middle > side.bottom)) continue;
    return toRect(r);
  }
  return null;
}

const core = createCelebrator({
  flair,
  mayMove,
  hasFocus: () => typeof document !== 'undefined' && document.hasFocus(),
  now: () => Date.now(),
  rowRect: (id) => (typeof document === 'undefined' ? null : visibleRowRect(id)),
  draw: (e) => drawEffect(e),
  later: (fn, ms) => void setTimeout(fn, ms),
});

/** The app's celebrator: `completion` for a finished reply, `merged` with the merge box's element. */
export const celebrate = {
  completion: core.completion,
  merged: (el: Element | null): void => core.merged(el ? toRect(el.getBoundingClientRect()) : null),
};
