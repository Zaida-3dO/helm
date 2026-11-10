// Your own colours (the `customColours` preference): an accent hue, a background tint and a
// contrast choice, laid over whichever skin is on. The value is a map keyed by profile id (or
// "default"), so each profile keeps its own.
//
// Readable contrast always: a colour is never refused, it is adjusted. The accent's lightness is
// stepped until the accent reads as text on every background and the best of white or black reads
// on the accent (4.5:1, or 7:1 on "more contrast"); a tint that would spoil the text is eased off.
// `solveColours` is the pure part (no DOM) so the table test can run every skin; `applyColours`
// reads the skin's own values, solves, and writes the overrides on the root element.
//
// The maths is small and local on purpose: sRGB, OKLab/OKLCH and the WCAG ratio, no dependency.

export type Tint = 'none' | 'soft' | 'strong';
export type Contrast = 'standard' | 'more';

export interface Colours {
  /** The accent's hue in degrees (0 to 359), or null to keep the skin's own accent. */
  accentHue: number | null;
  tint: Tint;
  contrast: Contrast;
}

export const COLOURS_DEFAULT: Colours = { accentHue: null, tint: 'none', contrast: 'standard' };
export const CUSTOM_COLOURS_PREF = 'customColours';
/** The map key used while no profile is active (or every profile is shown). */
export const COLOURS_DEFAULT_PROFILE = 'default';

/** The named hues Settings offers as swatches. */
export const COLOUR_PRESETS = [
  { id: 'red', title: 'Red', hue: 4 },
  { id: 'coral', title: 'Coral', hue: 18 },
  { id: 'amber', title: 'Amber', hue: 45 },
  { id: 'green', title: 'Green', hue: 145 },
  { id: 'teal', title: 'Teal', hue: 185 },
  { id: 'blue', title: 'Blue', hue: 235 },
  { id: 'violet', title: 'Violet', hue: 285 },
  { id: 'pink', title: 'Pink', hue: 340 },
] as const;

/** One entry of the stored map; each field is kept only when it is the right shape. */
export function parseColours(v: unknown): Colours {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const h = o.accentHue;
  return {
    accentHue: typeof h === 'number' && Number.isInteger(h) && h >= 0 && h <= 359 ? h : null,
    tint: o.tint === 'soft' || o.tint === 'strong' ? o.tint : 'none',
    contrast: o.contrast === 'more' ? 'more' : 'standard',
  };
}

/** The map key for the stored `activeProfile`: its id, or "default" when none is chosen or all are shown. */
export function colourProfileKey(activeProfile: unknown): string {
  return typeof activeProfile === 'string' && activeProfile !== '' && activeProfile !== 'all' && activeProfile.length <= 64 ? activeProfile : COLOURS_DEFAULT_PROFILE;
}

/** The colours for one profile out of the stored map. */
export function coloursFor(map: unknown, profile: string): Colours {
  if (!map || typeof map !== 'object' || !Object.hasOwn(map, profile)) return COLOURS_DEFAULT;
  return parseColours((map as Record<string, unknown>)[profile]);
}

export function isDefaultColours(c: Colours): boolean {
  return c.accentHue === null && c.tint === 'none' && c.contrast === 'standard';
}

/**
 * The map to store after one profile's colours change; the other profiles' entries stay as they are.
 * A profile back at the defaults leaves the map, and an empty map is `null` (the preference is cleared).
 */
export function withColours(map: unknown, profile: string, c: Colours): Record<string, unknown> | null {
  const next: Record<string, unknown> = map && typeof map === 'object' && !Array.isArray(map) ? { ...(map as Record<string, unknown>) } : {};
  if (isDefaultColours(c)) delete next[profile];
  else next[profile] = { accentHue: c.accentHue, tint: c.tint, contrast: c.contrast };
  return Object.keys(next).length === 0 ? null : next;
}

// --- colour maths ---------------------------------------------------------------------------

/** Whole numbers 0 to 255. */
export type Rgb = [number, number, number];
export type Lab = [number, number, number];

const WHITE: Rgb = [255, 255, 255];
const BLACK: Rgb = [0, 0, 0];

const toLinear = (c: number): number => {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};
const fromLinear = (l: number): number => {
  const s = l <= 0.0031308 ? l * 12.92 : 1.055 * l ** (1 / 2.4) - 0.055;
  return s * 255;
};

export function rgbToOklab([r, g, b]: Rgb): Lab {
  const lr = toLinear(r);
  const lg = toLinear(g);
  const lb = toLinear(b);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s, 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s, 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s];
}

/** Linear sRGB, not clamped: values outside 0 to 1 are outside the sRGB gamut. */
function oklabToLinear([L, a, b]: Lab): [number, number, number] {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s];
}

const clamp8 = (n: number): number => Math.min(255, Math.max(0, Math.round(n)));

export function oklabToRgb(lab: Lab): Rgb {
  const [r, g, b] = oklabToLinear(lab);
  return [clamp8(fromLinear(r)), clamp8(fromLinear(g)), clamp8(fromLinear(b))];
}

const lch = (L: number, C: number, H: number): Lab => [L, C * Math.cos((H * Math.PI) / 180), C * Math.sin((H * Math.PI) / 180)];

function inGamut(lab: Lab): boolean {
  return oklabToLinear(lab).every((v) => v >= -0.0005 && v <= 1.0005);
}

/** The sRGB colour of an OKLCH one; chroma is lowered, never the hue or lightness, until it fits the screen. */
export function oklchToRgb(L: number, C: number, H: number): Rgb {
  if (inGamut(lch(L, C, H))) return oklabToRgb(lch(L, C, H));
  let lo = 0;
  let hi = C;
  for (let i = 0; i < 20; i++) {
    const mid = (lo + hi) / 2;
    if (inGamut(lch(L, mid, H))) lo = mid;
    else hi = mid;
  }
  return oklabToRgb(lch(L, lo, H));
}

/** `t` of the way from `a` to `b`, mixed in OKLab. */
export function mixRgb(a: Rgb, b: Rgb, t: number): Rgb {
  const x = rgbToOklab(a);
  const y = rgbToOklab(b);
  return oklabToRgb([x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t]);
}

export function luminance([r, g, b]: Rgb): number {
  return 0.2126 * toLinear(r) + 0.7152 * toLinear(g) + 0.0722 * toLinear(b);
}

/** The WCAG contrast ratio of two colours (1 to 21). */
export function contrastRatio(a: Rgb, b: Rgb): number {
  const x = luminance(a);
  const y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

export function toHex(rgb: Rgb): string {
  return `#${rgb.map((n) => n.toString(16).padStart(2, '0')).join('')}`;
}

/** `#rgb`, `#rrggbb`, `#rrggbbaa` (alpha ignored) or `rgb(r, g, b)`; null for anything else. */
export function parseColour(text: string): Rgb | null {
  const s = text.trim().toLowerCase();
  if (s.startsWith('#')) {
    const h = s.slice(1);
    const full = h.length === 3 ? [...h].map((c) => c + c).join('') : h.length === 6 || h.length === 8 ? h.slice(0, 6) : '';
    if (full.length !== 6 || [...full].some((c) => !'0123456789abcdef'.includes(c))) return null;
    return [parseInt(full.slice(0, 2), 16), parseInt(full.slice(2, 4), 16), parseInt(full.slice(4, 6), 16)];
  }
  if (s.startsWith('rgb')) {
    const open = s.indexOf('(');
    const close = s.indexOf(')');
    if (open < 0 || close < open) return null;
    const parts = (s.slice(open + 1, close).split('/')[0] ?? '')
      .split(/[ ,]+/)
      .filter(Boolean)
      .slice(0, 3)
      .map(Number);
    if (parts.length !== 3 || parts.some((n) => !Number.isFinite(n))) return null;
    return [clamp8(parts[0]!), clamp8(parts[1]!), clamp8(parts[2]!)];
  }
  return null;
}

// --- solving ---------------------------------------------------------------------------------

/** The skin's own values the solver starts from, as read from the page. */
export interface ColourBase {
  bg: Rgb;
  sidebar: Rgb;
  surface: Rgb;
  surfaceAlt: Rgb;
  hover: Rgb;
  selected: Rgb;
  text: Rgb;
  secondary: Rgb;
  muted: Rgb;
  onSelected: Rgb;
}

const BASE_VARS: Record<keyof ColourBase, string> = {
  bg: '--bg',
  sidebar: '--sidebar',
  surface: '--surface',
  surfaceAlt: '--surface-alt',
  hover: '--hover',
  selected: '--selected',
  text: '--text',
  secondary: '--secondary',
  muted: '--muted',
  onSelected: '--on-selected',
};

type Backdrops = Pick<ColourBase, 'bg' | 'sidebar' | 'surface' | 'surfaceAlt' | 'hover' | 'selected'>;
/** The backgrounds body text sits on, and the narrower set muted text sits on. */
const BACKDROPS = ['bg', 'sidebar', 'surface', 'surfaceAlt', 'hover', 'selected'] as const;
const PAGES = ['bg', 'sidebar', 'surface', 'surfaceAlt'] as const;

/** Read the skin's values with `get` (a computed-style lookup); null when any is not a colour. */
export function readBase(get: (name: string) => string): ColourBase | null {
  const out: Partial<ColourBase> = {};
  for (const [key, name] of Object.entries(BASE_VARS) as [keyof ColourBase, string][]) {
    const c = parseColour(get(name));
    if (!c) return null;
    out[key] = c;
  }
  return out as ColourBase;
}

/** Every variable the colours can override; all are cleared before a new set is written. */
export const COLOUR_VARS = ['--accent', '--chrome-accent', '--accent-mark', '--accent-fill', '--fx-accent', '--on-accent-fill', '--accent-bg', '--chat-selected-fill', '--bg', '--sidebar', '--surface', '--surface-alt', '--hover', '--selected', '--muted', '--secondary'] as const;

export interface Solved {
  /** The variables to set, as hex colours. Empty when nothing is overridden. */
  vars: Record<string, string>;
  /** True when the result differs from what was asked so the text stays readable. */
  adjusted: boolean;
}

/** Where the accent starts looking for a lightness that reads, in OKLCH. */
const ACCENT_START = { light: 0.62, dark: 0.72 } as const;
const ACCENT_CHROMA = 0.16;
const TINT_START = { soft: 0.04, strong: 0.09 } as const;

export function solveColours(c: Colours, mode: 'light' | 'dark', base: ColourBase): Solved {
  const vars: Record<string, string> = {};
  let adjusted = false;
  const need = c.contrast === 'more' ? 7 : 4.5;
  const hue = c.accentHue;
  let back: Backdrops = { bg: base.bg, sidebar: base.sidebar, surface: base.surface, surfaceAlt: base.surfaceAlt, hover: base.hover, selected: base.selected };

  // Tint: wash the backgrounds with the hue, easing off while the text would read worse than it did.
  if (hue !== null && c.tint !== 'none') {
    const wash = oklchToRgb(0.6, 0.12, hue);
    const holds = (cand: Backdrops): boolean =>
      BACKDROPS.every((k) => contrastRatio(base.text, cand[k]) >= Math.min(need, contrastRatio(base.text, base[k]) - 0.05)) &&
      PAGES.every((k) => contrastRatio(base.muted, cand[k]) >= Math.min(4.5, contrastRatio(base.muted, base[k]) - 0.05)) &&
      contrastRatio(base.onSelected, cand.selected) >= Math.min(need, contrastRatio(base.onSelected, base.selected) - 0.05);
    let tinted = false;
    for (let w: number = TINT_START[c.tint]; w >= 0.01; w *= 0.75) {
      const cand = Object.fromEntries(BACKDROPS.map((k) => [k, mixRgb(base[k], wash, w)])) as Backdrops;
      if (holds(cand)) {
        back = cand;
        tinted = true;
        if (w !== TINT_START[c.tint]) adjusted = true;
        break;
      }
    }
    if (tinted) {
      vars['--bg'] = toHex(back.bg);
      vars['--sidebar'] = toHex(back.sidebar);
      vars['--surface'] = toHex(back.surface);
      vars['--surface-alt'] = toHex(back.surfaceAlt);
      vars['--hover'] = toHex(back.hover);
      vars['--selected'] = toHex(back.selected);
    } else {
      adjusted = true;
    }
  }

  // More contrast: pull the quieter text toward the main text until it reaches 7:1 on the pages.
  if (c.contrast === 'more') {
    for (const key of ['muted', 'secondary'] as const) {
      let pick: Rgb | null = null;
      for (let i = 0; i <= 20 && !pick; i++) {
        const cand = i === 0 ? base[key] : mixRgb(base[key], base.text, i / 20);
        if (i === 20 || PAGES.every((k) => contrastRatio(cand, back[k]) >= 7)) pick = cand;
      }
      if (pick && toHex(pick) !== toHex(base[key])) vars[BASE_VARS[key]] = toHex(pick);
    }
  }

  // Accent: step the lightness until it reads as text on every background and carries white or black text.
  if (hue !== null) {
    const dark = mode === 'dark';
    const start: number = dark ? ACCENT_START.dark : ACCENT_START.light;
    const accentBgShare = dark ? 0.16 : 0.1;
    const reads = (accent: Rgb): boolean => {
      const tintBg = mixRgb(back.bg, accent, accentBgShare);
      const onFill = contrastRatio(WHITE, accent) >= contrastRatio(BLACK, accent) ? WHITE : BLACK;
      return [...PAGES.map((k) => back[k]), tintBg].every((bg) => contrastRatio(accent, bg) >= need) && contrastRatio(onFill, accent) >= need;
    };
    let step = 0;
    let accent = oklchToRgb(start, ACCENT_CHROMA, hue);
    for (; step < 40 && !reads(accent); step++) {
      const next = start + (dark ? 1 : -1) * 0.02 * (step + 1);
      if (next < 0.05 || next > 0.98) break;
      accent = oklchToRgb(next, ACCENT_CHROMA, hue);
    }
    if (step > 0) adjusted = true;
    const onFill = contrastRatio(WHITE, accent) >= contrastRatio(BLACK, accent) ? WHITE : BLACK;
    const hex = toHex(accent);
    for (const name of ['--accent', '--chrome-accent', '--accent-mark', '--accent-fill', '--fx-accent']) vars[name] = hex;
    vars['--on-accent-fill'] = toHex(onFill);
    vars['--accent-bg'] = toHex(mixRgb(back.bg, accent, accentBgShare));
    // The selected chat row takes a share of the accent while its own text still reads.
    for (const share of [0.22, 0.16, 0.1, 0.05]) {
      const fill = mixRgb(back.selected, accent, share);
      if (contrastRatio(base.onSelected, fill) >= need) {
        vars['--chat-selected-fill'] = toHex(fill);
        break;
      }
    }
  }
  return { vars, adjusted };
}

/** Remove every override the colours wrote. */
export function clearColours(root: HTMLElement): void {
  for (const name of COLOUR_VARS) root.style.removeProperty(name);
}

/**
 * Apply `c` on top of the skin now on `root`. The overrides are cleared first so the skin's own
 * values are what the solver reads. Returns whether the result was adjusted to stay readable.
 */
export function applyColours(root: HTMLElement, c: Colours, mode: 'light' | 'dark'): { adjusted: boolean } {
  clearColours(root);
  if (isDefaultColours(c)) return { adjusted: false };
  const style = getComputedStyle(root);
  const base = readBase((name) => style.getPropertyValue(name));
  if (!base) return { adjusted: false };
  const { vars, adjusted } = solveColours(c, mode, base);
  for (const [name, value] of Object.entries(vars)) root.style.setProperty(name, value);
  return { adjusted };
}

/** The result of the last `applyColours`, for the Colours control to say "Adjusted to stay readable." */
let lastAdjusted = false;
const adjustedListeners = new Set<() => void>();

export function setColoursAdjusted(adjusted: boolean): void {
  if (adjusted === lastAdjusted) return;
  lastAdjusted = adjusted;
  for (const l of [...adjustedListeners]) l();
}

export function coloursAdjusted(): boolean {
  return lastAdjusted;
}

export function onColoursAdjusted(l: () => void): () => void {
  adjustedListeners.add(l);
  return () => {
    adjustedListeners.delete(l);
  };
}
