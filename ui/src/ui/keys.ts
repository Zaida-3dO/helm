// The one keybinding registry. Areas
// register named commands with a default chord; the user may rebind or clear any of them; one
// window listener dispatches. No area adds its own keydown handler for a shortcut.
//
// - Chords are written "Ctrl+Shift+K" (modifier order Ctrl, Alt, Shift, Super). Cmd reads as Ctrl
//   and Option as Alt, so chords written with either map across.
// - Reserved chords (Windows, WebView2) can never be bound.
// - A conflict (two commands on one chord) is reported, and the user's own binding wins over a
//   default; between two defaults the first registered keeps it.
// - Bindings with scope "global" are system-wide (the global hotkey). They live in the same map so
//   conflicts are found, but the window listener never dispatches them: the host registers them.

export type BindingScope = 'app' | 'global';

export interface Binding {
  id: string;
  title: string;
  /** The default chord, or null for an unbound command. */
  defaultKeys: string | null;
  scope?: BindingScope;
  run?: () => void;
  /** When false the chord is ignored (for example while a dialog is open). */
  enabled?: () => boolean;
  /** Fire even when focus is in a text field (most shortcuts with Ctrl do). Default true. */
  inInputs?: boolean;
}

/**
 * Chords Windows or WebView2 own. A *user* rebinding onto one is refused.
 * Defaults are exempt: the inspector toggle is Ctrl+Shift+I, which WebView2 only claims for
 * DevTools in debug builds. Every chord with Super (the Windows key) is reserved too.
 */
export const RESERVED = ['Alt+F4', 'Alt+Tab', 'Alt+Space', 'Ctrl+Alt+Delete', 'Ctrl+Escape', 'Ctrl+Shift+Escape', 'Ctrl+Shift+I', 'Ctrl+Shift+J', 'F12'] as const;

const isReserved = (chord: string) => (RESERVED as readonly string[]).includes(chord) || chord.split('+').slice(0, -1).includes('Super');

const MOD_ORDER = ['Ctrl', 'Alt', 'Shift', 'Super'] as const;

/** Normalise a chord; null when it is not a chord (no key, or an unknown modifier). */
export function normaliseChord(keys: string): string | null {
  const parts = keys
    .split('+')
    .map((p) => p.trim())
    .filter(Boolean);
  const key = parts.pop();
  if (!key) return null;
  const mods = new Set<string>();
  for (const p of parts) {
    const m = p.toLowerCase();
    if (m === 'ctrl' || m === 'control' || m === 'mod' || m === 'cmd' || m === 'cmdorctrl') mods.add('Ctrl');
    else if (m === 'alt' || m === 'option' || m === 'opt') mods.add('Alt');
    else if (m === 'shift') mods.add('Shift');
    else if (m === 'super' || m === 'win') mods.add('Super');
    else return null;
  }
  const k = key.length === 1 ? key.toUpperCase() : key.charAt(0).toUpperCase() + key.slice(1);
  return [...MOD_ORDER.filter((m) => mods.has(m)), k].join('+');
}

/** The chord a keydown spells, or null for a bare modifier. Uses the physical key for letters. */
export function chordOf(e: { key: string; code?: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }): string | null {
  if (['Control', 'Shift', 'Alt', 'Meta', 'OS'].includes(e.key)) return null;
  let key = e.key;
  if (e.code?.startsWith('Key')) key = e.code.slice(3);
  else if (e.code?.startsWith('Digit')) key = e.code.slice(5);
  else if (e.code === 'Backquote') key = '`';
  else if (e.code === 'Comma') key = ',';
  else if (e.code === 'Period') key = '.';
  else if (e.code === 'Slash') key = '/';
  else if (key === ' ') key = 'Space';
  const mods: string[] = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  return normaliseChord([...mods, key].join('+'));
}

export interface Conflict {
  chord: string;
  ids: string[];
}

type Listener = () => void;

export class KeyRegistry {
  /** The binding in force per id: the top of that id's stack. */
  private readonly bindings = new Map<string, Binding>();
  /** Every live registration per id, oldest first, so an unmount restores the previous holder. */
  private readonly stacks = new Map<string, Binding[]>();
  /** id → chord (null = cleared by the user). */
  private user: Record<string, string | null> = {};
  private readonly listeners = new Set<Listener>();
  private version = 0;

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  getVersion = (): number => this.version;

  private changed(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  register(b: Binding): () => void {
    // Two mounted views (main chat + side chat) may register one id: the latest wins, and when it
    // unregisters the earlier holder is back in force rather than the id vanishing.
    const stack = [...(this.stacks.get(b.id) ?? []), b];
    this.stacks.set(b.id, stack);
    this.bindings.set(b.id, b);
    this.changed();
    return () => {
      const cur = this.stacks.get(b.id) ?? [];
      if (!cur.includes(b)) return;
      const next = cur.filter((x) => x !== b);
      if (next.length === 0) {
        this.stacks.delete(b.id);
        this.bindings.delete(b.id);
      } else {
        this.stacks.set(b.id, next);
        this.bindings.set(b.id, next[next.length - 1]!);
      }
      this.changed();
    };
  }

  /** Load the user's map (the `keybindings` preference). Unknown shapes are ignored. */
  loadUserMap(v: unknown): void {
    const out: Record<string, string | null> = {};
    if (v && typeof v === 'object') {
      for (const [id, chord] of Object.entries(v as Record<string, unknown>)) {
        if (chord === null) out[id] = null;
        else if (typeof chord === 'string') {
          const n = normaliseChord(chord);
          if (n) out[id] = n;
        }
      }
    }
    this.user = out;
    this.changed();
  }

  userMap(): Record<string, string | null> {
    return { ...this.user };
  }

  /** The chord in force for a command. */
  chordFor(id: string): string | null {
    if (id in this.user) return this.user[id] ?? null;
    const b = this.bindings.get(id);
    return b?.defaultKeys ? normaliseChord(b.defaultKeys) : null;
  }

  list(): (Binding & { chord: string | null; custom: boolean })[] {
    return [...this.bindings.values()].map((b) => ({ ...b, chord: this.chordFor(b.id), custom: b.id in this.user }));
  }

  /**
   * Rebind (or clear with null) a command for the user. Refuses reserved chords and chords another
   * command already uses; the refusal names why. Returns the new user map to persist.
   */
  setUser(id: string, chord: string | null): { ok: true; map: Record<string, string | null> } | { ok: false; reason: string } {
    if (!this.bindings.has(id)) return { ok: false, reason: 'There is no such command.' };
    if (chord === null) {
      this.user = { ...this.user, [id]: null };
      this.changed();
      return { ok: true, map: this.userMap() };
    }
    const n = normaliseChord(chord);
    if (!n) return { ok: false, reason: `"${chord}" is not a shortcut.` };
    if (isReserved(n)) return { ok: false, reason: `${n} belongs to Windows or the web view and cannot be used.` };
    const taken = [...this.bindings.keys()].find((other) => other !== id && this.chordFor(other) === n);
    if (taken) return { ok: false, reason: `${n} is already used by "${this.bindings.get(taken)?.title}".` };
    this.user = { ...this.user, [id]: n };
    this.changed();
    return { ok: true, map: this.userMap() };
  }

  /** Back to the default. */
  reset(id: string): Record<string, string | null> {
    const { [id]: _drop, ...rest } = this.user;
    this.user = rest;
    this.changed();
    return this.userMap();
  }

  /** Chords claimed by more than one command. */
  conflicts(): Conflict[] {
    const by = new Map<string, string[]>();
    for (const id of this.bindings.keys()) {
      const c = this.chordFor(id);
      if (!c) continue;
      by.set(c, [...(by.get(c) ?? []), id]);
    }
    return [...by.entries()].filter(([, ids]) => ids.length > 1).map(([chord, ids]) => ({ chord, ids }));
  }

  /** The app-scope command bound to this chord, if any: a user binding wins, then registration order. */
  resolve(chord: string): Binding | null {
    let fallback: Binding | null = null;
    for (const b of this.bindings.values()) {
      if ((b.scope ?? 'app') !== 'app') continue;
      if (this.chordFor(b.id) !== chord) continue;
      if (b.enabled && !b.enabled()) continue;
      if (b.id in this.user) return b;
      fallback ??= b;
    }
    return fallback;
  }

  /** Dispatch one keydown. True when a command ran. */
  dispatch(e: { key: string; code?: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean; target?: EventTarget | null }): boolean {
    const chord = chordOf(e);
    if (!chord) return false;
    const b = this.resolve(chord);
    if (!b?.run) return false;
    const t = e.target as HTMLElement | null | undefined;
    const typing = !!t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable === true);
    const bare = !chord.includes('+');
    if (typing && (b.inInputs === false || bare)) return false;
    b.run();
    return true;
  }

  install(target: Pick<Window, 'addEventListener' | 'removeEventListener'>): () => void {
    const onKey = (ev: Event) => {
      const e = ev as KeyboardEvent;
      if (e.defaultPrevented || e.isComposing || e.repeat) return;
      if (this.dispatch(e)) e.preventDefault();
    };
    target.addEventListener('keydown', onKey);
    return () => target.removeEventListener('keydown', onKey);
  }
}

/** "Fork chat · Ctrl+Shift+F", or the bare label when the command has no chord. */
export function hintLabel(label: string, chord: string | null | undefined): string {
  return chord ? `${label} · ${chord}` : label;
}

/**
 * Whether a captured chord can be a shortcut the user sets: it needs Ctrl or Alt, unless it is a
 * function key. Shift alone would eat typed capitals; a bare key would eat typing.
 */
export function isAssignable(chord: string | null): boolean {
  if (!chord) return false;
  const parts = chord.split('+');
  const key = parts[parts.length - 1] ?? '';
  if (/^F([1-9]|1\d|2[0-4])$/.test(key)) return true;
  return parts.includes('Ctrl') || parts.includes('Alt');
}

/** The app's one registry. */
export const keys = new KeyRegistry();

export const KEYBINDINGS_PREF = 'keybindings';
