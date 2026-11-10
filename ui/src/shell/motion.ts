// One answer to "may this move?" for everything decorative (the pet, celebrations, flair).
//
// Nothing moves under the operating system's reduced-motion setting, at the quiet motion level
// (Zen), in the slow-display mode (`data-motion='low'` on the root), or, when "Pause animations
// while scrolling" is on, while the page is scrolling. The root's attributes are the source of
// truth (theme.ts writes `data-dopamine`, the frame-rate policy writes `data-motion`), so this file
// needs nothing from either of them; it watches the root and tells listeners when the answer or
// the level changes.

export type Flair = 'quiet' | 'standard' | 'maximal';

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

interface Win {
  matchMedia?: (q: string) => { matches: boolean; addEventListener: (k: 'change', h: () => void) => void; removeEventListener: (k: 'change', h: () => void) => void };
  addEventListener: (kind: string, h: () => void, capture?: boolean) => void;
  removeEventListener: (kind: string, h: () => void, capture?: boolean) => void;
  setTimeout: (h: () => void, ms: number) => unknown;
  clearTimeout: (id: unknown) => void;
}

/** How long after the last scroll event the page counts as still scrolling. */
export const SCROLL_IDLE_MS = 160;

const listeners = new Set<() => void>();
let root: HTMLElement | null = null;
let win: Win | null = null;
let pauseOnScroll = false;
let scrolling = false;

function reducedMotion(): boolean {
  return win?.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
}

/** The delight level as a word: quiet is Zen, maximal is the top level. */
export function flair(): Flair {
  const level = root?.dataset.dopamine;
  return level === 'zen' ? 'quiet' : level === 'brainrot' ? 'maximal' : 'standard';
}

/** Whether decoration may animate right now. */
export function mayMove(): boolean {
  if (reducedMotion()) return false;
  if (flair() === 'quiet') return false;
  if (root?.dataset.motion === 'low') return false;
  return !(pauseOnScroll && scrolling);
}

/** Listen for the answer or the level changing; returns the function that stops listening. */
export function onMotionChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

function changed(): void {
  for (const l of [...listeners]) l();
}

/**
 * Watch the root and the preferences. `root` and `win` are parameters so a test can stand in for
 * the page. Returns a stop function.
 */
export function connectMotion(api: ApiFn, bus: Bus, el: HTMLElement, w: Win = window as unknown as Win): () => void {
  root = el;
  win = w;
  pauseOnScroll = false;
  scrolling = false;
  let idle: unknown = null;
  const setPause = (v: unknown) => {
    pauseOnScroll = v === true;
    if (!pauseOnScroll) {
      scrolling = false;
      delete el.dataset.scrolling;
    }
    changed();
  };
  api<unknown>('preference_get', { key: 'pauseAnimationsOnScroll' })
    .then(setPause)
    .catch(() => undefined);
  const offPref = bus.on('preferences', (e) => {
    const c = (e.changed ?? {}) as Record<string, unknown>;
    if ('pauseAnimationsOnScroll' in c) setPause(c.pauseAnimationsOnScroll);
  });
  // Scroll events do not bubble, so listen in the capture phase to hear every scroller.
  const onScroll = () => {
    if (!pauseOnScroll) return;
    if (!scrolling) {
      scrolling = true;
      el.dataset.scrolling = '1';
      changed();
    }
    if (idle !== null) w.clearTimeout(idle);
    idle = w.setTimeout(() => {
      idle = null;
      scrolling = false;
      delete el.dataset.scrolling;
      changed();
    }, SCROLL_IDLE_MS);
  };
  w.addEventListener('scroll', onScroll, true);
  const mq = w.matchMedia?.('(prefers-reduced-motion: reduce)');
  mq?.addEventListener('change', changed);
  const observer = typeof MutationObserver === 'function' ? new MutationObserver(changed) : null;
  observer?.observe(el, { attributes: true, attributeFilter: ['data-dopamine', 'data-motion'] });
  return () => {
    offPref();
    w.removeEventListener('scroll', onScroll, true);
    mq?.removeEventListener('change', changed);
    observer?.disconnect();
    if (idle !== null) w.clearTimeout(idle);
    delete el.dataset.scrolling;
    if (root === el) {
      root = null;
      win = null;
      pauseOnScroll = false;
      scrolling = false;
    }
  };
}
