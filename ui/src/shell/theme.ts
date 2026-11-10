// Colour mode and skin: writes data-theme / data-skin / data-effects on <html>.
// The `appearance` preference is "system" | "light" | "dark"; anything else reads as system.
// The `interfaceTheme` preference names the skin written to data-skin; styles.css defines a
// colour set per skin per mode, so the appearance mode still picks light or dark.

export type Appearance = 'system' | 'light' | 'dark';

export function parseAppearance(v: unknown): Appearance {
  return v === 'light' || v === 'dark' ? v : 'system';
}

export const SKIN_DEFAULT = 'default';

/** The skins styles.css carries, in the order Settings shows them. */
export const SKINS = [
  'default',
  'retro-desktop',
  'team-chat',
  'browser',
  'terminal',
  'ide',
  'community',
  'forum',
  'encyclopedia',
] as const;

export type Skin = (typeof SKINS)[number];

/** The ids skins had before they were renamed; a stored old id reads as the new one. */
const LEGACY_SKINS: Record<string, Skin> = {
  'windows-xp': 'retro-desktop',
  slack: 'team-chat',
  chrome: 'browser',
  intellij: 'ide',
  discord: 'community',
  reddit: 'forum',
  wikipedia: 'encyclopedia',
};

/** The stored `interfaceTheme`; anything the host would not accept reads as the default skin. */
export function parseSkin(v: unknown): Skin {
  if (typeof v !== 'string') return SKIN_DEFAULT;
  if ((SKINS as readonly string[]).includes(v)) return v as Skin;
  return Object.hasOwn(LEGACY_SKINS, v) ? LEGACY_SKINS[v] : SKIN_DEFAULT;
}

export function resolveTheme(appearance: Appearance, prefersDark: boolean): 'light' | 'dark' {
  if (appearance === 'system') return prefersDark ? 'dark' : 'light';
  return appearance;
}

// How much movement and decoration the interface shows (the `dopamine` preference).
// A stored value is one of the level ids, or a document whose `mode` names the level;
// anything unreadable, and a custom set (no control editor yet), reads as standard.

export type Dopamine = 'zen' | 'standard' | 'brainrot';
export const DOPAMINE_DEFAULT: Dopamine = 'standard';

export function parseDopamine(v: unknown): Dopamine {
  const mode = typeof v === 'string' ? v : v && typeof v === 'object' ? (v as { mode?: unknown }).mode : undefined;
  return mode === 'zen' || mode === 'brainrot' ? mode : DOPAMINE_DEFAULT;
}

/**
 * Apply the theme attributes; returns a stop function for the OS colour-scheme listener.
 * The delight level rides along: Zen writes the quiet effects set as well, so the theme and the
 * level are always consistent however they changed.
 */
export function applyTheme(root: HTMLElement, appearance: Appearance, skin: Skin = SKIN_DEFAULT, level: Dopamine = DOPAMINE_DEFAULT): () => void {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const write = () => {
    root.dataset.theme = resolveTheme(appearance, mq.matches);
    root.dataset.skin = skin;
    root.dataset.dopamine = level;
    root.dataset.effects = level === 'zen' ? 'quiet' : 'full';
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
 * Read `appearance`, `interfaceTheme`, `dopamine`, `fontSizes` and `zoomLevel`, apply them, and
 * re-apply when a preference changes. Returns a stop function.
 */
export function connectAppearance(api: ApiFn, bus: Bus, root: HTMLElement): () => void {
  let appearance: Appearance = 'system';
  let skin: Skin = SKIN_DEFAULT;
  let level: Dopamine = DOPAMINE_DEFAULT;
  let stop = applyTheme(root, appearance, skin, level);
  const write = () => {
    stop();
    stop = applyTheme(root, appearance, skin, level);
  };
  const set = (v: unknown) => {
    appearance = parseAppearance(v);
    write();
  };
  const setSkin = (v: unknown) => {
    skin = parseSkin(v);
    write();
  };
  const setLevel = (v: unknown) => {
    level = parseDopamine(v);
    write();
  };
  api<unknown>('preference_get', { key: 'appearance' })
    .then(set)
    .catch(() => undefined);
  api<unknown>('preference_get', { key: 'interfaceTheme' })
    .then(setSkin)
    .catch(() => undefined);
  api<unknown>('preference_get', { key: 'dopamine' })
    .then(setLevel)
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
    if ('interfaceTheme' in changed) setSkin(changed.interfaceTheme);
    if ('dopamine' in changed) setLevel(changed.dopamine);
    if ('fontSizes' in changed) applyFontSizes(root, parseFontSizes(changed.fontSizes));
    if ('zoomLevel' in changed) applyZoom(api, root, parseZoom(changed.zoomLevel));
  });
  return () => {
    off();
    stop();
  };
}
