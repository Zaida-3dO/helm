import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MAX_VOICES, SOUNDS_DEFAULT, connectSounds, cueGain, cueNotes, cueVoices, parseSounds, playCue, resetCueEngine, setCueClock, type Sounds } from './sounds.ts';

// The module reaches for `window` for the audio unlock; stand in for the browser globals.
const listeners = new Map<string, (() => void)[]>();
(globalThis as { window?: unknown }).window = {
  addEventListener: (kind: string, h: () => void) => listeners.set(kind, [...(listeners.get(kind) ?? []), h]),
  removeEventListener: (kind: string) => listeners.delete(kind),
};

interface FakeOsc {
  freq: number;
  started: number | null;
  glideTo: number | null;
}
// One fake context for the whole file: sounds.ts caches its AudioContext after the first use,
// so every test shares it and clears `created` instead of installing a fresh one.
const noise: { count: number } = { count: 0 };
const fakeCtx = {
  currentTime: 1,
  state: 'suspended',
  sampleRate: 8000,
  destination: {},
  created: [] as FakeOsc[],
  resume: () => Promise.resolve(),
  createOscillator: () => {
    const osc: FakeOsc = { freq: 0, started: null, glideTo: null };
    fakeCtx.created.push(osc);
    return {
      connect: (x: unknown) => x,
      type: 'sine',
      frequency: {
        set value(f: number) {
          osc.freq = f;
        },
        setValueAtTime: () => undefined,
        exponentialRampToValueAtTime: (to: number) => (osc.glideTo = to),
      },
      start: (t: number) => (osc.started = t),
      stop: () => undefined,
    };
  },
  createGain: () => ({ connect: (x: unknown) => x, gain: { setValueAtTime: () => undefined, exponentialRampToValueAtTime: () => undefined } }),
  createBuffer: (_channels: number, length: number) => ({ getChannelData: () => new Float32Array(length) }),
  createBufferSource: () => {
    noise.count++;
    return { connect: (x: unknown) => x, start: () => undefined };
  },
  createBiquadFilter: () => ({ connect: (x: unknown) => x, type: '', frequency: { value: 0 }, Q: { value: 0 } }),
};
(globalThis as { AudioContext?: unknown }).AudioContext = function () {
  return fakeCtx;
};
const reset = () => {
  fakeCtx.created.length = 0;
  noise.count = 0;
};

// A clock the tests move by hand, so the 35 ms gap and the voice count are exact.
let now = 0;
const step = (ms = 1000) => (now += ms);
setCueClock(() => now);
const S = (o: Partial<Sounds> = {}): Sounds => ({ ...SOUNDS_DEFAULT, ...o });
const sounded = () => fakeCtx.created.length + noise.count;

test('a stored sounds object keeps only well-shaped fields', () => {
  assert.deepEqual(parseSounds(undefined), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ enabled: false, volume: 0, themed: false }), S({ enabled: false, volume: 0, themed: false }));
  assert.deepEqual(parseSounds({ volume: 101 }), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ volume: 45.5 }), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ enabled: 'yes', volume: 7 }), S({ enabled: true, volume: 7 }));
  assert.deepEqual(parseSounds({ loud: true }), SOUNDS_DEFAULT);
  assert.deepEqual(parseSounds({ cues: true, focused: true }), S({ cues: true, focused: true }));
  assert.deepEqual(parseSounds({ cues: 'on', focused: 1 }), SOUNDS_DEFAULT);
  assert.equal(SOUNDS_DEFAULT.cues, false);
  assert.equal(SOUNDS_DEFAULT.focused, false);
});

test('volume scales the peak gain, and 0 is silence', () => {
  assert.equal(cueGain(0), 0);
  assert.equal(cueGain(100), 0.22);
  assert.ok(Math.abs(cueGain(45) - 0.099) < 1e-9);
});

test('the cues: the themed chimes are two notes; unthemed is one beep; the small cues are original shapes', () => {
  assert.equal(cueNotes('complete', true).length, 2);
  const done = cueNotes('complete', true);
  assert.ok(done[1]!.freq > done[0]!.freq, 'the completion chime rises');
  assert.ok(done[1]!.at > 0, 'the second note waits for the first');
  const att = cueNotes('attention', true);
  assert.equal(att.length, 2);
  assert.ok(att[1]!.freq < att[0]!.freq, 'the attention cue falls');
  assert.deepEqual(cueNotes('complete', false), cueNotes('attention', false));
  assert.equal(cueNotes('complete', false).length, 1);
  // The small cues, whatever `themed` says.
  const send = cueVoices('send', true)[0]!;
  assert.deepEqual([send.kind, send.dur], ['noise', 0.12]);
  assert.deepEqual(cueNotes('send', true), [], 'send is noise, not a pitch');
  assert.deepEqual(cueNotes('tick', false), [{ freq: 1500, at: 0, dur: 0.015 }]);
  const err = cueVoices('error', true)[0]!;
  assert.ok(err.kind === 'tone' && err.freq === 220 && err.to === 165 && err.dur === 0.18, 'error glides 220 to 165 Hz over 180 ms');
  const bounce = cueVoices('bounce', true)[0]!;
  assert.ok(bounce.kind === 'tone' && bounce.freq === 520 && bounce.glide === 0.04 && bounce.to! < 520, 'bounce drops in pitch over 40 ms');
});

test('playCue is silent when off or at volume 0; otherwise every note sounds', () => {
  reset();
  resetCueEngine();
  assert.equal(playCue('complete', S({ enabled: false })), false);
  assert.equal(playCue('complete', S({ volume: 0 })), false);
  assert.equal(sounded(), 0);
  step();
  assert.equal(playCue('complete', S({ volume: 80 })), true);
  assert.deepEqual(fakeCtx.created.map((o) => o.freq), cueNotes('complete', true).map((n) => n.freq));
  assert.ok(fakeCtx.created.every((o) => o.started !== null && o.started > 1));
});

test('small cues: send and tick need the cues switch; error and bounce do not; each makes its sound', () => {
  resetCueEngine();
  reset();
  step();
  assert.equal(playCue('send', S()), false, 'cues are off by default');
  assert.equal(playCue('tick', S()), false);
  assert.equal(sounded(), 0);
  assert.equal(playCue('send', S({ cues: true })), true);
  assert.equal(noise.count, 1, 'send is filtered noise');
  step();
  assert.equal(playCue('tick', S({ cues: true })), true);
  assert.equal(fakeCtx.created.at(-1)!.freq, 1500);
  step();
  assert.equal(playCue('error', S()), true);
  assert.deepEqual([fakeCtx.created.at(-1)!.freq, fakeCtx.created.at(-1)!.glideTo], [220, 165]);
  step();
  assert.equal(playCue('bounce', S()), true);
  assert.deepEqual([fakeCtx.created.at(-1)!.freq, fakeCtx.created.at(-1)!.glideTo], [520, 390]);
});

test('engine: a cue within 35 ms of the last is dropped, and no more than four voices run at once', () => {
  resetCueEngine();
  reset();
  step();
  assert.equal(playCue('tick', S({ cues: true })), true);
  step(34);
  assert.equal(playCue('tick', S({ cues: true })), false, 'inside the 35 ms gap');
  step(1);
  assert.equal(playCue('tick', S({ cues: true })), true, 'the gap is over');
  // Two-note chimes: two of them fill four voices, a third has to wait until they have ended.
  step();
  reset();
  assert.equal(playCue('complete', S()), true);
  step(40);
  assert.equal(playCue('attention', S()), true);
  step(40);
  assert.equal(playCue('complete', S()), false, `over ${MAX_VOICES} voices`);
  assert.equal(fakeCtx.created.length, 4);
  step();
  assert.equal(playCue('complete', S()), true, 'the earlier voices have ended');
});

test('away cues: completion chimes once, unfocused (or with focused on), only if fresh and unread; attention only for a chat not seen before; a preference change re-reads', () => {
  resetCueEngine();
  reset();
  const handlers = new Map<string, (e: Record<string, unknown>) => void>();
  const bus = {
    on: (kind: string, h: (e: Record<string, unknown>) => void) => {
      handlers.set(kind, h);
      return () => handlers.delete(kind);
    },
  };
  let focused = true;
  const pref: Record<string, unknown> = { sounds: { enabled: true, volume: 60, themed: true }, dopamine: 'standard' };
  const api = <T>(op: string, args?: unknown) => {
    assert.equal(op, 'preference_get');
    return Promise.resolve(pref[(args as { key: string }).key] as T);
  };
  const stop = connectSounds(api, bus, () => focused);
  const done = (id: string, extra: Record<string, unknown> = {}) => ({ completion: { id, chatId: `chat-${id}`, completedAt: Date.now(), read: false, dismissed: false, ...extra } });
  const att = (ids: string[]) => ({ summary: { needsYou: ids.map((chatId) => ({ chatId })), unread: [] } });

  focused = true;
  handlers.get('completion')!(done('a'));
  handlers.get('attention')!(att(['b']));
  assert.equal(fakeCtx.created.length, 0, 'nothing plays while the window has the focus');

  focused = false;
  step();
  handlers.get('completion')!(done('a'));
  assert.equal(fakeCtx.created.length, 2, 'the completion chime: two notes');
  step();
  handlers.get('completion')!(done('a'));
  assert.equal(fakeCtx.created.length, 2, 'the same completion never chimes twice');
  step();
  handlers.get('completion')!(done('stale', { completedAt: Date.now() - 31_000 }));
  handlers.get('completion')!(done('seen', { read: true }));
  handlers.get('completion')!(done('gone', { dismissed: true }));
  assert.equal(fakeCtx.created.length, 2, 'an old, read or dismissed completion is silent');
  fakeCtx.created.length = 0;
  step();
  handlers.get('attention')!(att(['b', 'c']));
  assert.equal(fakeCtx.created.length, 2, 'the attention cue fires once for the new chats');
  step();
  handlers.get('attention')!(att(['b', 'c']));
  assert.equal(fakeCtx.created.length, 2, 'the same chats do not sound again');
  // A chat that finished and is merely unread does not cue again (its completion already did).
  handlers.get('attention')!({ summary: { needsYou: [], unread: [{ chatId: 'x' }] } });
  assert.equal(fakeCtx.created.length, 2, 'an unread chat is not a needs-you cue');
  // 'b' left the list when it was answered; asking again later sounds again.
  step();
  handlers.get('attention')!(att(['b']));
  assert.equal(fakeCtx.created.length, 4, 'a chat that needs you again sounds again');

  // "Play even when Helm is in front" lets the chimes through with the window focused.
  focused = true;
  fakeCtx.created.length = 0;
  step();
  handlers.get('completion')!(done('front'));
  assert.equal(fakeCtx.created.length, 0);
  handlers.get('preferences')!({ changed: { sounds: { enabled: true, volume: 60, themed: true, cues: false, focused: true } } });
  step();
  handlers.get('completion')!(done('front'));
  assert.equal(fakeCtx.created.length, 2, 'focused: the chime plays in front too');
  focused = false;

  // Turning sounds off in Settings reaches the cues at once (the host event carries the object).
  fakeCtx.created.length = 0;
  handlers.get('preferences')!({ changed: { sounds: { enabled: false, volume: 60, themed: true } } });
  step();
  handlers.get('completion')!(done('d'));
  assert.equal(fakeCtx.created.length, 0);

  // The quiet delight level silences cues even with sounds switched back on, and raising it sounds again.
  handlers.get('preferences')!({ changed: { sounds: { enabled: true, volume: 60, themed: true }, dopamine: 'zen' } });
  step();
  handlers.get('completion')!(done('e'));
  assert.equal(fakeCtx.created.length, 0, 'Zen silences every cue');
  assert.equal(playCue('error'), false, 'and the small ones');
  handlers.get('preferences')!({ changed: { dopamine: 'standard' } });
  step();
  handlers.get('completion')!(done('f'));
  assert.equal(fakeCtx.created.length, 2, 'the standard level sounds again');

  stop();
  assert.ok(!handlers.has('completion') && !handlers.has('attention') && !handlers.has('preferences'));
});
