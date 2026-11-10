// The theme a page is given: the app's colours and fonts under the page's own names, plus the
// mode and the reduced-motion setting. Values are read from the live document so a skin or a
// light/dark switch carries through; anything that is not a plain value is left out.

export interface PageTheme {
  mode: 'light' | 'dark';
  variables: Record<string, string>;
  reducedMotion: boolean;
}

/** The page's variable names and the app tokens they read. */
export const THEME_VARIABLES: Record<string, string> = {
  '--helm-bg': '--bg',
  '--helm-text': '--text',
  '--helm-surface': '--surface',
  '--helm-muted': '--muted',
  '--helm-accent': '--accent',
  '--helm-border': '--line-strong',
  '--helm-font': '--font-ui',
  '--helm-mono': '--font-mono',
};

const PLAIN_VALUE = /^[^;{}<>\\]*$/;

/** A token value safe to hand over: short, and with no url(), script or block syntax in it. */
export function plainValue(v: string): string | null {
  const t = v.trim();
  if (!t || t.length > 200 || !PLAIN_VALUE.test(t) || /url\(|expression\(|@import/i.test(t)) return null;
  return t;
}

export function buildTheme(read: (token: string) => string, dark: boolean, reducedMotion: boolean): PageTheme {
  const variables: Record<string, string> = {};
  for (const [name, token] of Object.entries(THEME_VARIABLES)) {
    const v = plainValue(read(token));
    if (v) variables[name] = v;
  }
  return { mode: dark ? 'dark' : 'light', variables, reducedMotion };
}

/** The theme as the document shows it now. */
export function pageTheme(root: HTMLElement = document.documentElement): PageTheme {
  const style = getComputedStyle(root);
  const media = (q: string) => typeof matchMedia === 'function' && matchMedia(q).matches;
  const dark = root.dataset.theme ? root.dataset.theme === 'dark' : media('(prefers-color-scheme: dark)');
  return buildTheme((t) => style.getPropertyValue(t), dark, media('(prefers-reduced-motion: reduce)'));
}
