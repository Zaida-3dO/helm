import { test } from 'node:test';
import assert from 'node:assert/strict';
import { SOUNDS_DEFAULT, connectSounds, cueGain, cueNotes, parseSounds, playCue } from './sounds.ts';

// The module reaches for `window` for the audio unlock; stand in for the browser globals.
const listeners = new Map<string, (() => void)[]>();
(globalThis as { window?: unknown }).window = {
  addEventListener: (kind: string, h: () => void) => listeners.set(kind, [...(listeners.get(kind) ?? []), h]),
  removeEventListener: (kind: string) => listeners.delete(kind),
};

interface FakeOsc {
  freq: number;
  started: number | null;
}
// One fake context for the whole file: sounds.ts caches its AudioContext after the first use,
// so every test shares it and clears `created` instead of installing a fresh one.
const fakeCtx: {
  currentTime: number;
  state: string;
  destination: unknown;
  created: FakeOsc[];
  resume: () => Promise<void>;
  createOscillator: () => unknown;
  createGain: () => unknown;
} = {
  currentTime: 1,
  state: 'suspended',
  destination: {},
  created: [],
  resume: () => Promise.resolve(),
  createOscillator: () => {
    const osc: FakeOsc = { freq: 0, started: null };
    fakeCtx.created.push(osc);
    const env = { connect: (x: unknown) => x, setValueAtTime: () => undefined, exponentialRampToValueAtTime: () => undefined };
    return { ...env, type: 'sine', frequency: { set value(f: number) { osc.freq = f; } }, start: (t: number) => (osc.started = t), stop: () => undefined };
  },
  createGain: () => ({ connect: (x: unknown) => x, gain: { setValueAtTime: () => undefined, exponentialRampToValueAtTime: () => undefined } }),
};
(globalThis as { AudioContext?: unknown }).AudioContext = function () {
  return fakeCtx;
};
const reset = () => (fakeCtx.created.length = 0);

test('a stored sounds object keeps only well-shaped fields', () => {
  assert.deepEqual(parseSounds(undefined), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ enabled: false, volume: 0, themed: false }), { enabled: false, volume: 0, themed: false });
  assert.deepEqual(parseSounds({ volume: 101 }), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ volume: 45.5 }), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ enabled: 'yes', volume: 7 }), { enabled: true, volume: 7, themed: true });
  assert.deepEqual(parseSounds({ loud: true }), SOUNDS_DEFAULT);
});

test('volume scales the peak gain, and 0 is silence', () => {
  assert.equal(cueGain(0), 0);
  assert.equal(cueGain(100), 0.22);
  assert.ok(Math.abs(cueGain(45) - 0.099) < 1e-9);
});

test('the themed cues are two notes each; unthemed is one plain beep', () => {
  assert.equal(cueNotes('complete', true).length, 2);
  const done = cueNotes('complete', true);
  assert.ok(done[1]!.freq > done[0]!.freq, 'the completion chime rises');
  assert.ok(done[1]!.at > 0, 'the second note waits for the first');
  const att = cueNotes('attention', true);
  assert.equal(att.length, 2);
  assert.ok(att[1]!.freq < att[0]!.freq, 'the attention cue falls');
  assert.deepEqual(cueNotes('complete', false), cueNotes('attention', false));
  assert.equal(cueNotes('complete', false).length, 1);
});

test('playCue is silent when off or at volume 0; otherwise every note sounds', () => {
  reset();
  playCue('complete', { enabled: false, volume: 45, themed: true });
  assert.equal(fakeCtx.created.length, 0);
  playCue('complete', { enabled: true, volume: 0, themed: true });
  assert.equal(fakeCtx.created.length, 0);
  playCue('complete', { enabled: true, volume: 80, themed: true });
  assert.deepEqual(fakeCtx.created.map((o) => o.freq), cueNotes('complete', true).map((n) => n.freq));
  assert.ok(fakeCtx.created.every((o) => o.started !== null && o.started > 1));
});

test('away cues: completion plays only unfocused, attention only for a chat not seen before, and a preference change re-reads', () => {
  reset();
  const handlers = new Map<string, (e: Record<string, unknown>) => void>();
  const bus = {
    on: (kind: string, h: (e: Record<string, unknown>) => void) => {
      handlers.set(kind, h);
      return () => handlers.delete(kind);
    },
  };
  let focused = true;
  let pref = { enabled: true, volume: 60, themed: true };
  const api = (<T>(op: string) => {
    assert.equal(op, 'preference_get');
    return Promise.resolve(pref as T);
  });
  const stop = connectSounds(api, bus, () => focused);
  const done = (id: string) => ({ completion: { chatId: id } });
  const att = (ids: string[]) => ({ summary: { unread: ids.map((chatId) => ({ chatId })) } });

  focused = true;
  handlers.get('completion')!(done('a'));
  handlers.get('attention')!(att(['b']));
  assert.equal(fakeCtx.created.length, 0, 'nothing plays while the window has the focus');

  focused = false;
  handlers.get('completion')!(done('a'));
  assert.equal(fakeCtx.created.length, 2, 'the completion chime: two notes');
  fakeCtx.created.length = 0;
  handlers.get('attention')!(att(['b', 'c']));
  assert.equal(fakeCtx.created.length, 2, 'the attention cue fires once for the new chats');
  handlers.get('attention')!(att(['b', 'c']));
  assert.equal(fakeCtx.created.length, 2, 'the same chats do not sound again');

  // Turning sounds off in Settings reaches the cues at once (the host event carries the object).
  fakeCtx.created.length = 0;
  handlers.get('preferences')!({ changed: { sounds: { enabled: false, volume: 60, themed: true } } });
  handlers.get('completion')!(done('d'));
  assert.equal(fakeCtx.created.length, 0);

  stop();
  assert.ok(!handlers.has('completion') && !handlers.has('attention') && !handlers.has('preferences'));
});
