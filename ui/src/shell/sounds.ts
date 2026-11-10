// Interface sounds (the `sounds` preference): quiet Web Audio cues. The tones are synthesised, so
// nothing ships as an asset, and the volume knob is the gain of each voice.
//
// Away chimes: a two-note chime when a chat finishes and a shorter pair when a chat turns up
// needing attention, played only while the window is unfocused (unless "Play even when Helm is in
// front", `focused`, is on). `themed` picks the chimes; off gives one plain neutral beep.
// Small cues: `send`, `tick`, `error` and `bounce`, which other parts of the interface play with
// `playCue`. `send` and `tick` are interface sounds and play only when `cues` is on.
//
// Engine rules: at most four voices at once, a cue within 35 ms of the last is dropped, a
// completion chimes once and only while it is under 30 s old and still unread. The Zen delight
// level (the `dopamine` preference) silences every cue, whatever `sounds` says; the operating
// system's reduced-motion setting does not, because a cue is not motion.

import { parseDopamine } from './theme.ts';

export interface Sounds {
  enabled: boolean;
  volume: number; // 0..100
  themed: boolean;
  /** Small interface cues (send, tick). Off by default. */
  cues: boolean;
  /** Also play the away chimes while Helm is in front. Off by default. */
  focused: boolean;
}

export const SOUNDS_DEFAULT: Sounds = { enabled: true, volume: 45, themed: true, cues: false, focused: false };

/** A stored `sounds` object; each field is kept only when it is the right shape. */
export function parseSounds(v: unknown): Sounds {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  return {
    enabled: typeof o.enabled === 'boolean' ? o.enabled : SOUNDS_DEFAULT.enabled,
    volume: typeof o.volume === 'number' && Number.isInteger(o.volume) && o.volume >= 0 && o.volume <= 100 ? o.volume : SOUNDS_DEFAULT.volume,
    themed: typeof o.themed === 'boolean' ? o.themed : SOUNDS_DEFAULT.themed,
    cues: typeof o.cues === 'boolean' ? o.cues : SOUNDS_DEFAULT.cues,
    focused: typeof o.focused === 'boolean' ? o.focused : SOUNDS_DEFAULT.focused,
  };
}

export type SoundCue = 'complete' | 'attention' | 'send' | 'tick' | 'error' | 'bounce';

/** Cues that are small interface sounds: they play only when `cues` is on. */
const INTERFACE_CUES: readonly SoundCue[] = ['send', 'tick'];

export const MAX_VOICES = 4;
export const MIN_CUE_GAP_MS = 35;
/** A completion older than this no longer chimes. */
export const COMPLETION_MAX_AGE_MS = 30_000;
const COMPLETION_MEMORY = 1024;

interface Note {
  freq: number;
  at: number; // seconds from the cue start
  dur: number;
}

export type Voice =
  | { kind: 'tone'; freq: number; at: number; dur: number; level: number; /** Glide to this pitch over `glide` seconds. */ to?: number; glide?: number }
  | { kind: 'noise'; at: number; dur: number; level: number; /** The band the noise is filtered to. */ hz: number };

/** The voices of a cue; the notes were picked by ear, quiet and out of the way. */
export function cueVoices(cue: SoundCue, themed: boolean): Voice[] {
  switch (cue) {
    case 'send':
      return [{ kind: 'noise', at: 0, dur: 0.12, level: 0.3, hz: 1800 }];
    case 'tick':
      return [{ kind: 'tone', freq: 1500, at: 0, dur: 0.015, level: 0.7 }];
    case 'error':
      return [{ kind: 'tone', freq: 220, at: 0, dur: 0.18, level: 0.6, to: 165, glide: 0.18 }];
    case 'bounce':
      return [{ kind: 'tone', freq: 520, at: 0, dur: 0.09, level: 0.8, to: 390, glide: 0.04 }];
    default: {
      const notes: Note[] = !themed
        ? [{ freq: 660, at: 0, dur: 0.16 }]
        : cue === 'complete'
          ? [
              { freq: 523.25, at: 0, dur: 0.13 },
              { freq: 659.25, at: 0.14, dur: 0.22 },
            ]
          : [
              { freq: 783.99, at: 0, dur: 0.09 },
              { freq: 587.33, at: 0.1, dur: 0.15 },
            ];
      return notes.map((n): Voice => ({ kind: 'tone', ...n, level: 1 }));
    }
  }
}

/** The pitched notes of a cue (the noise of `send` has none). */
export function cueNotes(cue: SoundCue, themed: boolean): Note[] {
  const out: Note[] = [];
  for (const v of cueVoices(cue, themed)) if (v.kind === 'tone') out.push({ freq: v.freq, at: v.at, dur: v.dur });
  return out;
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

// What the engine knows between calls: the settings (kept current by `connectSounds`), the delight
// level, when the last cue sounded and when each running voice ends.
let current = { sounds: SOUNDS_DEFAULT, zen: false };
let lastCueAt = -Infinity;
let voiceEnds: number[] = [];
const realClock = () => (typeof performance !== 'undefined' ? performance.now() : Date.now());
let clock: () => number = realClock;

/** Tests only: stand in for the clock the debounce and the voice count use (null restores it). */
export function setCueClock(fn: (() => number) | null): void {
  clock = fn ?? realClock;
}

/** Tests only: forget the last cue, the running voices and the settings. */
export function resetCueEngine(): void {
  current = { sounds: SOUNDS_DEFAULT, zen: false };
  lastCueAt = -Infinity;
  voiceEnds = [];
}

function sound(ac: AudioContext, v: Voice, t0: number, gain: number): void {
  const g = ac.createGain();
  const attack = Math.min(0.015, v.dur * 0.3);
  g.gain.setValueAtTime(0.0001, t0 + v.at);
  g.gain.exponentialRampToValueAtTime(gain * v.level, t0 + v.at + attack);
  g.gain.exponentialRampToValueAtTime(0.0001, t0 + v.at + v.dur);
  g.connect(ac.destination);
  if (v.kind === 'noise') {
    const length = Math.max(1, Math.ceil(ac.sampleRate * v.dur));
    const buffer = ac.createBuffer(1, length, ac.sampleRate);
    const data = buffer.getChannelData(0);
    for (let i = 0; i < length; i++) data[i] = Math.random() * 2 - 1;
    const src = ac.createBufferSource();
    src.buffer = buffer;
    const band = ac.createBiquadFilter();
    band.type = 'bandpass';
    band.frequency.value = v.hz;
    band.Q.value = 0.7;
    src.connect(band);
    band.connect(g);
    src.start(t0 + v.at);
    return;
  }
  const osc = ac.createOscillator();
  osc.type = 'sine';
  osc.frequency.value = v.freq;
  if (v.to !== undefined) {
    osc.frequency.setValueAtTime(v.freq, t0 + v.at);
    osc.frequency.exponentialRampToValueAtTime(v.to, t0 + v.at + (v.glide ?? v.dur));
  }
  osc.connect(g);
  osc.start(t0 + v.at);
  osc.stop(t0 + v.at + v.dur + 0.02);
}

/**
 * Play one cue with the current settings (or `s`); returns whether it sounded. Nothing plays when
 * sounds are off, silent or Zen, for a small interface cue while `cues` is off, within 35 ms of the
 * last cue, when it would need more than four voices at once, or with no audio available (tests,
 * headless runs). It does not look at window focus: the away chimes do that themselves.
 */
export function playCue(cue: SoundCue, s: Sounds = current.sounds): boolean {
  if (current.zen || !s.enabled) return false;
  const gain = cueGain(s.volume);
  if (gain <= 0) return false;
  if (INTERFACE_CUES.includes(cue) && !s.cues) return false;
  const now = clock();
  if (now - lastCueAt < MIN_CUE_GAP_MS) return false;
  voiceEnds = voiceEnds.filter((end) => end > now);
  const voices = cueVoices(cue, s.themed);
  if (voiceEnds.length + voices.length > MAX_VOICES) return false;
  const ac = audioCtx();
  if (!ac) return false;
  const t0 = ac.currentTime + 0.02;
  for (const v of voices) {
    sound(ac, v, t0, gain);
    voiceEnds.push(now + (0.02 + v.at + v.dur + 0.02) * 1000);
  }
  lastCueAt = now;
  return true;
}

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

/**
 * Read `sounds`, play the cues on the `completion` and `attention` events while the window is
 * unfocused (or always, with `focused` on), and re-read when a preference changes. Returns a stop
 * function. `hasFocus` is a parameter so the tests can stand in for the window.
 */
export function connectSounds(api: ApiFn, bus: Bus, hasFocus: () => boolean = () => document.hasFocus()): () => void {
  current = { sounds: SOUNDS_DEFAULT, zen: false };
  const seenAttention = new Set<string>();
  const chimed = new Set<string>(); // completion ids already chimed, oldest first
  api<unknown>('preference_get', { key: 'sounds' })
    .then((v) => (current.sounds = parseSounds(v)))
    .catch(() => undefined);
  api<unknown>('preference_get', { key: 'dopamine' })
    .then((v) => (current.zen = parseDopamine(v) === 'zen'))
    .catch(() => undefined);
  // Open the audio context now, so the first cue after launch does not have to create it while the
  // window is in the background. Where the web view still holds it suspended until the page has had
  // one interaction, the first click or key opens it.
  void audioCtx();
  const unlock = () => void audioCtx();
  window.addEventListener('pointerdown', unlock, { once: true });
  window.addEventListener('keydown', unlock, { once: true });
  const away = () => !hasFocus() || current.sounds.focused;
  const offC = bus.on('completion', (e) => {
    const c = e.completion as { id?: unknown; chatId?: unknown; completedAt?: unknown; read?: unknown; dismissed?: unknown } | undefined;
    if (!c || typeof c.chatId !== 'string') return;
    // Already read or dismissed, or too old to be news: no chime.
    if (c.read === true || c.dismissed === true) return;
    if (typeof c.completedAt === 'number' && Date.now() - c.completedAt > COMPLETION_MAX_AGE_MS) return;
    const key = typeof c.id === 'string' ? c.id : c.chatId;
    if (chimed.has(key) || !away()) return;
    chimed.add(key);
    if (chimed.size > COMPLETION_MEMORY) chimed.delete(chimed.values().next().value as string);
    playCue('complete');
  });
  const offA = bus.on('attention', (e) => {
    // Chats that need an answer (a question, an approval). A chat that merely finished is the
    // completion cue's business, so it does not sound twice.
    const rows = (e.summary as { needsYou?: { chatId?: unknown }[] } | undefined)?.needsYou ?? [];
    const now = new Set<string>();
    for (const r of rows) if (typeof r?.chatId === 'string') now.add(r.chatId);
    // A chat that no longer needs you may need you again later, and sounds again then.
    for (const id of seenAttention) if (!now.has(id)) seenAttention.delete(id);
    let fresh = false;
    for (const id of now) {
      if (!seenAttention.has(id)) {
        seenAttention.add(id);
        fresh = true;
      }
    }
    if (fresh && away()) playCue('attention');
  });
  const offP = bus.on('preferences', (e) => {
    const changed = (e.changed ?? {}) as Record<string, unknown>;
    if ('sounds' in changed) current.sounds = parseSounds(changed.sounds);
    if ('dopamine' in changed) current.zen = parseDopamine(changed.dopamine) === 'zen';
  });
  return () => {
    offC();
    offA();
    offP();
    window.removeEventListener('pointerdown', unlock);
    window.removeEventListener('keydown', unlock);
    current = { sounds: SOUNDS_DEFAULT, zen: false };
  };
}
