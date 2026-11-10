// Dictation in the composer: how a key or a button press becomes "start" and "finish", what the
// microphone is turned into (a 16 kHz mono WAV, made here in the page), and where the words go.
// The pure rules (the press model, the downsampler, the WAV encoder, the microphone pick, the
// insertion) are tested; `Recorder` is the thin layer over the browser's audio and is not.
//
// The page may not reach the network, so the recording goes to the host (`dictation_transcribe`),
// which posts it to the speech-to-text server the user set up.

import type { Api } from '../api/client.ts';
import { ApiError } from '../api/client.ts';
import { chordOf } from '../ui/keys.ts';
import { bytesToBase64 } from './attachments.ts';

export const SAMPLE_RATE = 16_000;
/** A press shorter than this is a tap (toggle); longer is hold-to-talk. */
export const TAP_MS = 350;
/** The longest recording; the host takes about this much. */
export const MAX_SECONDS = 300;
export const LEVEL_BARS = 5;
/** How long an error stays in the pill. */
export const ERROR_MS = 6_000;
export const DICTATE_BINDING = 'composer.dictate';
export const DICTATE_KEYS = 'Ctrl+Shift+Space';

// ---- the press model ------------------------------------------------------------------------

export type KeyPhase = 'idle' | 'holding' | 'toggled' | 'swallow';
export interface KeyState {
  phase: KeyPhase;
  /** When the press that started this phase came. */
  since: number;
}
/** `stop`: the recording ended some other way (Enter, Esc, the time cap, an error). */
export type KeyInput = 'press' | 'release' | 'stop';
export type KeyAction = 'start' | 'finish' | null;

export const IDLE: KeyState = { phase: 'idle', since: 0 };

/**
 * One step of the one-key rule. A tap (a press and release within `TAP_MS`) starts listening and
 * keeps going; a second press finishes. A longer press talks only while it is held and finishes on
 * release. The mouse button and the key share it. Pure: the caller keeps the state.
 */
export function keyStep(s: KeyState, input: KeyInput, now: number): [KeyState, KeyAction] {
  switch (input) {
    case 'stop':
      return [IDLE, null];
    case 'press':
      if (s.phase === 'idle') return [{ phase: 'holding', since: now }, 'start'];
      // The second tap finishes; its release then does nothing.
      if (s.phase === 'toggled') return [{ phase: 'swallow', since: now }, 'finish'];
      // Key repeat while held.
      return [s, null];
    case 'release':
      if (s.phase === 'holding') return now - s.since < TAP_MS ? [{ phase: 'toggled', since: s.since }, null] : [IDLE, 'finish'];
      if (s.phase === 'swallow') return [IDLE, null];
      return [s, null];
  }
}

/** Whether releasing this key ends the press of `chord` (its main key or one of its modifiers). */
export function releasesChord(chord: string | null, e: { key: string; code?: string }): boolean {
  if (!chord) return false;
  const parts = chord.split('+');
  const main = parts.pop() ?? '';
  const mod = e.key === 'Control' ? 'Ctrl' : e.key === 'Shift' ? 'Shift' : e.key === 'Alt' ? 'Alt' : e.key === 'Meta' || e.key === 'OS' ? 'Super' : null;
  if (mod) return parts.includes(mod);
  return chordOf({ key: e.key, code: e.code, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false }) === main;
}

// ---- turning sound into a WAV ---------------------------------------------------------------

/**
 * Averages a stream of samples down to `outRate` (never up: a slower microphone keeps its own
 * rate). An integer phase decides where each output sample ends, so 48 kHz and 44.1 kHz both
 * land on exactly 16 kHz over time. The mean is a simple low-pass, enough for speech.
 */
export class Resampler {
  readonly outRate: number;
  private readonly inRate: number;
  private phase = 0;
  private sum = 0;
  private count = 0;

  constructor(inRate: number, outRate = SAMPLE_RATE) {
    this.inRate = Math.round(inRate);
    this.outRate = Math.min(this.inRate, outRate);
  }

  /** The 16-bit samples this chunk completes. */
  push(chunk: Float32Array): Int16Array {
    const out: number[] = [];
    for (let i = 0; i < chunk.length; i++) {
      this.sum += chunk[i]!;
      this.count++;
      this.phase += this.outRate;
      if (this.phase >= this.inRate) {
        this.phase -= this.inRate;
        out.push(toPcm(this.sum / this.count));
        this.sum = 0;
        this.count = 0;
      }
    }
    return Int16Array.from(out);
  }
}

function toPcm(v: number): number {
  const c = Math.max(-1, Math.min(1, v));
  return Math.round(c < 0 ? c * 0x8000 : c * 0x7fff);
}

/** A mono 16-bit PCM WAV file. */
export function encodeWav(pcm: Int16Array, rate = SAMPLE_RATE): Uint8Array {
  const data = pcm.length * 2;
  const out = new Uint8Array(44 + data);
  const v = new DataView(out.buffer);
  const tag = (at: number, s: string) => {
    for (let i = 0; i < s.length; i++) out[at + i] = s.charCodeAt(i);
  };
  tag(0, 'RIFF');
  v.setUint32(4, 36 + data, true);
  tag(8, 'WAVE');
  tag(12, 'fmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  tag(36, 'data');
  v.setUint32(40, data, true);
  for (let i = 0; i < pcm.length; i++) v.setInt16(44 + i * 2, pcm[i]!, true);
  return out;
}

/** The recording's length in seconds, from its WAV header. */
export function wavSeconds(wav: Uint8Array): number {
  if (wav.length < 44) return 0;
  const v = new DataView(wav.buffer, wav.byteOffset, wav.byteLength);
  const rate = v.getUint32(24, true);
  return rate ? (wav.length - 44) / 2 / rate : 0;
}

/** "0:07" */
export function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

// ---- the microphone -------------------------------------------------------------------------

const VIRTUAL_IDS = new Set(['default', 'communications']);

function plainLabel(l: string): string {
  return l
    .toLowerCase()
    .replace(/^(default|communications)\s*-\s*/, '')
    .replace(/\s+/g, ' ')
    .trim();
}

/**
 * The device id of the microphone the user chose in Settings, matched by its name (the browser's
 * labels carry the same friendly names, sometimes with a "Default - " in front). Null when none
 * matches: the default microphone is used. A real device wins over the "default" alias.
 */
export function pickMic(devices: ReadonlyArray<{ kind: string; deviceId: string; label: string }>, wanted: string | null): string | null {
  const want = wanted ? plainLabel(wanted) : '';
  if (!want) return null;
  const inputs = devices.filter((d) => d.kind === 'audioinput' && d.deviceId && d.label);
  const near = (d: { label: string }) => {
    const l = plainLabel(d.label);
    return l === want || (want.length >= 4 && (l.includes(want) || want.includes(l)));
  };
  const hits = inputs.filter(near);
  const exact = hits.filter((d) => plainLabel(d.label) === want);
  const pool = exact.length ? exact : hits;
  return (pool.find((d) => !VIRTUAL_IDS.has(d.deviceId)) ?? pool[0])?.deviceId ?? null;
}

/** The microphone could not be opened (none, blocked or in use). */
export class MicError extends Error {
  constructor() {
    super("Helm can't hear a microphone. Check Settings → Shell → Microphone.");
  }
}

type AudioCtor = new (o?: AudioContextOptions) => AudioContext;

/**
 * Records the microphone as 16 kHz mono sound until `finish` or `cancel`. It holds the microphone
 * only between those calls. Uses a script processor rather than a worklet: a worklet's module
 * would be a second script for the page's content security policy to allow.
 */
export class Recorder {
  private ctx: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private node: ScriptProcessorNode | null = null;
  private analyser: AnalyserNode | null = null;
  private bins = new Uint8Array(0);
  private resampler: Resampler | null = null;
  private chunks: Int16Array[] = [];
  private samples = 0;
  private startedAt = 0;
  private capped = false;

  /** Open the microphone (the named one when it is found) and begin. `onCap` fires at the time limit. */
  async start(micName: string | null, onCap: () => void): Promise<void> {
    const md = typeof navigator === 'undefined' ? undefined : navigator.mediaDevices;
    if (!md?.getUserMedia) throw new MicError();
    const open = async (deviceId?: string) => {
      try {
        return await md.getUserMedia({
          audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true, ...(deviceId ? { deviceId: { exact: deviceId } } : {}) },
        });
      } catch {
        throw new MicError();
      }
    };
    let stream = await open();
    if (micName) {
      // Labels appear only once the microphone has been opened, hence the second look.
      const id = pickMic(await md.enumerateDevices().catch(() => []), micName);
      if (id && id !== stream.getAudioTracks()[0]?.getSettings().deviceId) {
        const chosen = await open(id).catch(() => null);
        if (chosen) {
          stream.getTracks().forEach((t) => t.stop());
          stream = chosen;
        }
      }
    }
    this.stream = stream;
    try {
      const audio = globalThis as unknown as { AudioContext?: AudioCtor; webkitAudioContext?: AudioCtor };
      const Ctor = audio.AudioContext ?? audio.webkitAudioContext;
      if (!Ctor) throw new MicError();
      // Ask for 16 kHz so the browser does the resampling; whatever it gives is handled below.
      let ctx: AudioContext;
      try {
        ctx = new Ctor({ sampleRate: SAMPLE_RATE });
      } catch {
        ctx = new Ctor();
      }
      this.ctx = ctx;
      await ctx.resume().catch(() => undefined);
      const resampler = new Resampler(ctx.sampleRate);
      this.resampler = resampler;
      const source = ctx.createMediaStreamSource(stream);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 256;
      analyser.smoothingTimeConstant = 0.55;
      this.bins = new Uint8Array(analyser.frequencyBinCount);
      this.analyser = analyser;
      const node = ctx.createScriptProcessor(4096, 1, 1);
      node.onaudioprocess = (e) => {
        if (this.capped) return;
        const pcm = resampler.push(e.inputBuffer.getChannelData(0));
        this.chunks.push(pcm);
        this.samples += pcm.length;
        if (this.samples >= MAX_SECONDS * resampler.outRate) {
          this.capped = true;
          onCap();
        }
      };
      source.connect(analyser);
      source.connect(node);
      // A processor only runs while it is connected on; it writes no output, so nothing is heard.
      node.connect(ctx.destination);
      this.node = node;
      this.startedAt = Date.now();
    } catch (e) {
      this.release();
      throw e instanceof MicError ? e : new MicError();
    }
  }

  elapsedMs(): number {
    return this.startedAt ? Date.now() - this.startedAt : 0;
  }

  /** Five levels from 0 to 1, low voices to high, for the bars. */
  levels(): number[] {
    const a = this.analyser;
    if (!a) return Array(LEVEL_BARS).fill(0);
    a.getByteFrequencyData(this.bins);
    // Roughly 120 Hz to 4 kHz, where speech lives, in five groups.
    const lo = 2;
    const hi = Math.min(this.bins.length, 64);
    const size = Math.max(1, Math.floor((hi - lo) / LEVEL_BARS));
    return Array.from({ length: LEVEL_BARS }, (_, i) => {
      let sum = 0;
      for (let j = lo + i * size; j < lo + (i + 1) * size; j++) sum += this.bins[j] ?? 0;
      return Math.min(1, (sum / size / 255) * 1.8);
    });
  }

  /** Stop and return the recording as a WAV. */
  finish(): Uint8Array {
    const rate = this.resampler?.outRate ?? SAMPLE_RATE;
    const all = new Int16Array(this.samples);
    let at = 0;
    for (const c of this.chunks) {
      all.set(c, at);
      at += c.length;
    }
    this.release();
    return encodeWav(all, rate);
  }

  /** Stop and throw the recording away. */
  cancel(): void {
    this.release();
  }

  private release(): void {
    if (this.node) this.node.onaudioprocess = null;
    try {
      this.node?.disconnect();
      this.analyser?.disconnect();
    } catch {
      // Already disconnected.
    }
    this.stream?.getTracks().forEach((t) => t.stop());
    void this.ctx?.close().catch(() => undefined);
    this.node = this.analyser = this.ctx = this.stream = null;
    this.chunks = [];
    this.samples = 0;
    this.startedAt = 0;
  }
}

// ---- the host ---------------------------------------------------------------------------------

export interface DictationView {
  sttUrl: string | null;
  allowPrivateNetwork: boolean;
  model: string;
  language: string;
  configured: boolean;
  speechReserveGb: number;
  speechReserveMaxGb: number;
}

export interface DictationSettingsChange {
  sttUrl?: string;
  allowPrivateNetwork?: boolean;
  model?: string;
  language?: string;
  speechReserveGb?: number;
}

export function dictationOps(api: Api) {
  return {
    get: () => api<DictationView>('dictation_settings_get', {}),
    set: (change: DictationSettingsChange) => api<DictationView>('dictation_settings_set', change),
    test: () => api<{ ok: boolean; message: string }>('dictation_test', {}),
    /** The recording as the host takes it: base64 in `wavBase64`. */
    transcribe: (wav: Uint8Array) => api<{ text: string }>('dictation_transcribe', { wavBase64: bytesToBase64(wav) }),
  };
}

/** What the pill says for a failure, and what the user can do about it. */
export interface Problem {
  message: string;
  /** The recording is kept: send it again. */
  retry: boolean;
  /** Offer a link to Settings → Shell. */
  settings: boolean;
}

export const NO_MIC = "Helm can't hear a microphone. Check Settings → Shell → Microphone.";
export const NOT_SET_UP = "Dictation isn't set up yet.";
export const NO_ANSWER = "Speech-to-text didn't answer. Your recording is kept: Retry.";
export const NOTHING_HEARD = "Didn't catch anything.";
export const TOO_LONG = 'That recording is too long to send. Dictation takes up to five minutes.';

/** The problem a failed call stands for. */
export function problemOf(e: unknown): Problem {
  if (e instanceof MicError) return { message: NO_MIC, retry: false, settings: true };
  const code = e instanceof ApiError ? e.code : '';
  if (code === 'not_set_up') return { message: NOT_SET_UP, retry: false, settings: true };
  // The saved address stopped passing the rule; the host says why.
  if (code === 'bad_address' && e instanceof ApiError) return { message: e.message, retry: false, settings: true };
  if (code === 'too_big') return { message: TOO_LONG, retry: false, settings: false };
  if (code === 'bad_audio') return { message: "That recording couldn't be read. Try again.", retry: false, settings: false };
  // Nothing answered, or it answered with an error: the audio is still here to send again.
  return { message: NO_ANSWER, retry: true, settings: false };
}

// ---- the words ----------------------------------------------------------------------------------

/**
 * The text to put at the caret: a space first when the caret follows a word and the words do not
 * begin with punctuation. `caret` is null when nothing is placed (the words go to the end).
 */
export function spokenInsert(draft: string, caret: number | null, spoken: string): string {
  const words = spoken.trim();
  if (!words) return '';
  const at = caret ?? draft.length;
  const before = at > 0 ? draft[at - 1] : undefined;
  const needsSpace = before !== undefined && !/\s/.test(before) && !/^[.,;:!?)\]}%]/.test(words);
  return needsSpace ? ` ${words}` : words;
}
