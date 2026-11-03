// Interface sounds (the `sounds` preference): quiet Web Audio cues, played only while the window
// is unfocused — a two-note chime when a chat finishes and a shorter pair when a chat turns up
// needing attention. The tones are synthesised, so nothing ships as an asset and the volume knob
// is the oscillator's own gain. `themed` picks the chimes; off gives one plain neutral beep.

export interface Sounds {
  enabled: boolean;
  volume: number; // 0..100
  themed: boolean;
}

export const SOUNDS_DEFAULT: Sounds = { enabled: true, volume: 45, themed: true };

/** A stored `sounds` object; each field is kept only when it is the right shape. */
export function parseSounds(v: unknown): Sounds {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  return {
    enabled: typeof o.enabled === 'boolean' ? o.enabled : SOUNDS_DEFAULT.enabled,
    volume: typeof o.volume === 'number' && Number.isInteger(o.volume) && o.volume >= 0 && o.volume <= 100 ? o.volume : SOUNDS_DEFAULT.volume,
    themed: typeof o.themed === 'boolean' ? o.themed : SOUNDS_DEFAULT.themed,
  };
}

export type SoundCue = 'complete' | 'attention';

interface Note {
  freq: number;
  at: number; // seconds from the cue start
  dur: number;
}

/** The notes of a cue; the notes were picked by ear, quiet and out of the way. */
export function cueNotes(cue: SoundCue, themed: boolean): Note[] {
  if (!themed) return [{ freq: 660, at: 0, dur: 0.16 }];
  if (cue === 'complete')
    return [
      { freq: 523.25, at: 0, dur: 0.13 },
      { freq: 659.25, at: 0.14, dur: 0.22 },
    ];
  return [
    { freq: 783.99, at: 0, dur: 0.09 },
    { freq: 587.33, at: 0.1, dur: 0.15 },
  ];
}

/** Peak gain for a volume level; the cue stays quiet even at 100. 0 means silence. */
export function cueGain(volume: number): number {
  return volume <= 0 ? 0 : 0.22 * (volume / 100);
}

let ctx: AudioContext | null = null;

function audioCtx(): AudioContext | null {
  const Ctor = (globalThis as { AudioContext?: typeof AudioContext }).AudioContext;
  if (!Ctor) return null;
  if (!ctx) {
    try {
      ctx = new Ctor();
    } catch {
      return null;
    }
  }
  if (ctx.state === 'suspended') void ctx.resume().catch(() => undefined);
  return ctx;
}

/** Play one cue; a no-op when off, silent, or with no audio available (tests, headless runs). */
export function playCue(cue: SoundCue, s: Sounds): void {
  if (!s.enabled) return;
  const gain = cueGain(s.volume);
  if (gain <= 0) return;
  const ac = audioCtx();
  if (!ac) return;
  const t0 = ac.currentTime + 0.02;
  for (const n of cueNotes(cue, s.themed)) {
    const osc = ac.createOscillator();
    const g = ac.createGain();
    osc.type = 'sine';
    osc.frequency.value = n.freq;
    g.gain.setValueAtTime(0.0001, t0 + n.at);
    g.gain.exponentialRampToValueAtTime(gain, t0 + n.at + 0.015);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + n.at + n.dur);
    osc.connect(g).connect(ac.destination);
    osc.start(t0 + n.at);
    osc.stop(t0 + n.at + n.dur + 0.02);
  }
}

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

/**
 * Read `sounds`, play the cues on the `completion` and `attention` events while the window is
 * unfocused, and re-read when a preference changes. Returns a stop function. `hasFocus` is a
 * parameter so the tests can stand in for the window.
 */
export function connectSounds(api: ApiFn, bus: Bus, hasFocus: () => boolean = () => document.hasFocus()): () => void {
  let sounds = SOUNDS_DEFAULT;
  const seenAttention = new Set<string>();
  api<unknown>('preference_get', { key: 'sounds' })
    .then((v) => (sounds = parseSounds(v)))
    .catch(() => undefined);
  // The audio context starts suspended until the page has had one interaction; open it on the
  // first click or key so the next away-from-window cue is heard.
  const unlock = () => void audioCtx();
  window.addEventListener('pointerdown', unlock, { once: true });
  window.addEventListener('keydown', unlock, { once: true });
  const offC = bus.on('completion', (e) => {
    const c = e.completion as { chatId?: unknown } | undefined;
    if (c && typeof c.chatId === 'string' && !hasFocus()) playCue('complete', sounds);
  });
  const offA = bus.on('attention', (e) => {
    const rows = (e.summary as { unread?: { chatId?: unknown }[] } | undefined)?.unread ?? [];
    let fresh = false;
    for (const r of rows) {
      if (typeof r?.chatId === 'string' && !seenAttention.has(r.chatId)) {
        seenAttention.add(r.chatId);
        fresh = true;
      }
    }
    if (fresh && !hasFocus()) playCue('attention', sounds);
  });
  const offP = bus.on('preferences', (e) => {
    const changed = (e.changed ?? {}) as Record<string, unknown>;
    if ('sounds' in changed) sounds = parseSounds(changed.sounds);
  });
  return () => {
    offC();
    offA();
    offP();
    window.removeEventListener('pointerdown', unlock);
    window.removeEventListener('keydown', unlock);
  };
}
