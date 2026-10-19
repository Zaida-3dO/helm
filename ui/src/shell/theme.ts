// Colour mode: writes data-theme / data-skin / data-effects on <html>.
// The `appearance` preference is "system" | "light" | "dark"; anything else reads as system.

export type Appearance = 'system' | 'light' | 'dark';

export function parseAppearance(v: unknown): Appearance {
  return v === 'light' || v === 'dark' ? v : 'system';
}

export function resolveTheme(appearance: Appearance, prefersDark: boolean): 'light' | 'dark' {
  if (appearance === 'system') return prefersDark ? 'dark' : 'light';
  return appearance;
}

/** Apply the theme attributes; returns a stop function for the OS colour-scheme listener. */
export function applyTheme(root: HTMLElement, appearance: Appearance): () => void {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const write = () => {
    root.dataset.theme = resolveTheme(appearance, mq.matches);
    root.dataset.skin = 'default';
    root.dataset.effects = 'full';
  };
  write();
  mq.addEventListener('change', write);
  return () => mq.removeEventListener('change', write);
}

// Text sizes and zoom. The interface size also
// scales the interface unit (styles.css derives --ui-unit from it).

export interface FontSizes {
  interface: number;
  response: number;
  review: number;
}

export const FONT_DEFAULTS: FontSizes = { interface: 13, response: 14, review: 10 };
export const FONT_RANGES: Record<keyof FontSizes, [number, number]> = { interface: [11, 20], response: [11, 24], review: [9, 20] };
export const ZOOM_DEFAULT = 100;

const inRange = (v: unknown, [lo, hi]: [number, number], dflt: number) => (typeof v === 'number' && Number.isInteger(v) && v >= lo && v <= hi ? v : dflt);

/** A stored `fontSizes` object, each size kept only when it is a whole number in range. */
export function parseFontSizes(v: unknown): FontSizes {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  return {
    interface: inRange(o.interface, FONT_RANGES.interface, FONT_DEFAULTS.interface),
    response: inRange(o.response, FONT_RANGES.response, FONT_DEFAULTS.response),
    review: inRange(o.review, FONT_RANGES.review, FONT_DEFAULTS.review),
  };
}

/** A stored `zoomLevel`: 50-200 in steps of 10, else 100. */
export function parseZoom(v: unknown): number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 50 && v <= 200 && v % 10 === 0 ? v : ZOOM_DEFAULT;
}

/** The CSS variables the sizes set: the review line height is the review size + 11. */
export function fontVars(s: FontSizes): Record<string, string> {
  return {
    '--interface-font-size': `${s.interface}px`,
    '--response-font-size': `${s.response}px`,
    '--review-font-size': `${s.review}px`,
    '--review-line-height': `${s.review + 11}px`,
  };
}

export function applyFontSizes(root: HTMLElement, s: FontSizes): void {
  for (const [k, v] of Object.entries(fontVars(s))) root.style.setProperty(k, v);
}

/**
 * Zoom the whole window: the host sets the web view's own zoom (`window_zoom`), so the page keeps
 * plain CSS pixels and popups, resizers and the pointer need no compensation. Any CSS zoom left on
 * <html> by an earlier build is cleared.
 */
export function applyZoom(api: ApiFn, root: HTMLElement, level: number): void {
  root.style.zoom = '';
  api('window_zoom', { level }).catch(() => undefined);
}

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

/**
 * Read `appearance`, `fontSizes` and `zoomLevel`, apply them, and re-apply when a preference
 * changes. Returns a stop function.
 */
export function connectAppearance(api: ApiFn, bus: Bus, root: HTMLElement): () => void {
  let stop = applyTheme(root, 'system');
  const set = (v: unknown) => {
    stop();
    stop = applyTheme(root, parseAppearance(v));
  };
  api<unknown>('preference_get', { key: 'appearance' })
    .then(set)
    .catch(() => undefined);
  api<unknown>('preference_get', { key: 'fontSizes' })
    .then((v) => applyFontSizes(root, parseFontSizes(v)))
    .catch(() => undefined);
  api<unknown>('preference_get', { key: 'zoomLevel' })
    .then((v) => applyZoom(api, root, parseZoom(v)))
    .catch(() => undefined);
  const off = bus.on('preferences', (e) => {
    const changed = (e.changed ?? {}) as Record<string, unknown>;
    if ('appearance' in changed) set(changed.appearance);
    if ('fontSizes' in changed) applyFontSizes(root, parseFontSizes(changed.fontSizes));
    if ('zoomLevel' in changed) applyZoom(api, root, parseZoom(changed.zoomLevel));
  });
  return () => {
    off();
    stop();
  };
}
