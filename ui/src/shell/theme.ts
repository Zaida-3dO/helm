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

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

/** Read `appearance`, apply it, and re-apply when the preference changes. Returns a stop function. */
export function connectAppearance(api: ApiFn, bus: Bus, root: HTMLElement): () => void {
  let stop = applyTheme(root, 'system');
  const set = (v: unknown) => {
    stop();
    stop = applyTheme(root, parseAppearance(v));
  };
  api<unknown>('preference_get', { key: 'appearance' })
    .then(set)
    .catch(() => undefined);
  const off = bus.on('preferences', (e) => {
    const changed = (e.changed ?? {}) as Record<string, unknown>;
    if ('appearance' in changed) set(changed.appearance);
  });
  return () => {
    off();
    stop();
  };
}
