import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError } from '../api/client.ts';
import {
  clock,
  encodeWav,
  IDLE,
  keyStep,
  MicError,
  NO_ANSWER,
  NO_MIC,
  NOT_SET_UP,
  pickMic,
  problemOf,
  releasesChord,
  Resampler,
  spokenInsert,
  TAP_MS,
  wavSeconds,
  type KeyAction,
  type KeyInput,
} from './dictation.ts';

/** Run presses and releases through the rule; the actions it asked for, in order. */
function run(steps: Array<[KeyInput, number]>): KeyAction[] {
  let s = IDLE;
  return steps.map(([input, at]) => {
    const [next, action] = keyStep(s, input, at);
    s = next;
    return action;
  });
}

test('a tap toggles listening and holding talks until release', () => {
  for (const [name, steps, want] of [
    ['tap to start, tap to finish', [['press', 0], ['release', 90], ['press', 5000], ['release', 5080], ['press', 6000]], ['start', null, 'finish', null, 'start']],
    ['hold to talk', [['press', 0], ['press', 40], ['press', 80], ['release', 2500], ['release', 2501]], ['start', null, null, 'finish', null]],
    ['the line between a tap and a hold', [['press', 0], ['release', TAP_MS - 1], ['press', 1000]], ['start', null, 'finish']],
    ['exactly the tap time is a hold', [['press', 0], ['release', TAP_MS]], ['start', 'finish']],
    ['Enter or Esc ends it, and a late release does nothing', [['press', 0], ['release', 50], ['stop', 3000], ['release', 3100], ['press', 4000]], ['start', null, null, null, 'start']],
    ['a release with nothing pressed', [['release', 0], ['stop', 1]], [null, null]],
  ] as Array<[string, Array<[KeyInput, number]>, KeyAction[]]>) {
    assert.deepEqual(run(steps), want, name);
  }
  // The key and the button are one rule: the chord is let go by its main key or any modifier.
  for (const [chord, key, code, want] of [
    ['Ctrl+Shift+Space', ' ', 'Space', true],
    ['Ctrl+Shift+Space', 'Control', 'ControlLeft', true],
    ['Ctrl+Shift+Space', 'Shift', 'ShiftRight', true],
    ['Ctrl+Shift+Space', 'Alt', 'AltLeft', false],
    ['Ctrl+Shift+Space', 'a', 'KeyA', false],
    ['Ctrl+Alt+D', 'd', 'KeyD', true],
    ['Ctrl+Alt+D', 'Meta', 'MetaLeft', false],
    [null, ' ', 'Space', false],
  ] as Array<[string | null, string, string, boolean]>) {
    assert.equal(releasesChord(chord, { key, code }), want, `${chord} / ${key}`);
  }
});

test('the recording becomes a 16 kHz mono WAV', () => {
  // 48 kHz takes the mean of every three samples; values clip at full scale.
  const r = new Resampler(48_000);
  assert.deepEqual([...r.push(Float32Array.from([0.3, 0.3, 0.3, -0.3, -0.3, -0.3]))], [9830, -9830]);
  assert.deepEqual([...r.push(Float32Array.from([2, 2, 2, -2, -2]))], [32767], 'the sixth sample is still to come');
  assert.deepEqual([...r.push(Float32Array.from([-2]))], [-32768]);
  // 44.1 kHz does not divide evenly, yet a second of it is exactly 16,000 samples, in any chunking.
  const odd = new Resampler(44_100);
  const total = [17, 4096, 20_000, 44_100 - 17 - 4096 - 20_000].reduce((n, size) => n + odd.push(new Float32Array(size).fill(0.5)).length, 0);
  assert.equal(total, 16_000);
  // A slower microphone keeps its own rate: nothing is invented.
  const slow = new Resampler(8_000);
  assert.equal(slow.outRate, 8_000);
  assert.equal(slow.push(new Float32Array(100)).length, 100);
  assert.equal(new Resampler(16_000).outRate, 16_000);

  const pcm = Int16Array.from([0, 1, -1, 32767, -32768]);
  const wav = encodeWav(pcm);
  const v = new DataView(wav.buffer);
  const tag = (at: number, n: number) => String.fromCharCode(...wav.subarray(at, at + n));
  assert.deepEqual([tag(0, 4), tag(8, 8), tag(36, 4)], ['RIFF', 'WAVEfmt ', 'data']);
  assert.deepEqual(
    [v.getUint32(4, true), v.getUint16(20, true), v.getUint16(22, true), v.getUint32(24, true), v.getUint32(28, true), v.getUint16(34, true), v.getUint32(40, true), wav.length],
    [36 + 10, 1, 1, 16_000, 32_000, 16, 10, 54],
  );
  assert.deepEqual([...new Int16Array(wav.buffer.slice(44))], [...pcm], 'the samples follow the header, little-endian');
  assert.equal(wavSeconds(encodeWav(new Int16Array(16_000))), 1);
  assert.equal(wavSeconds(encodeWav(new Int16Array(4_000), 8_000)), 0.5);
  assert.equal(wavSeconds(new Uint8Array(10)), 0);
  for (const [ms, want] of [[0, '0:00'], [7_400, '0:07'], [65_000, '1:05'], [299_999, '4:59'], [-5, '0:00']] as Array<[number, string]>) {
    assert.equal(clock(ms), want);
  }
});

test('the microphone, the words and the problems', () => {
  const dev = (deviceId: string, label: string, kind = 'audioinput') => ({ kind, deviceId, label });
  const devices = [
    dev('default', 'Default - Microphone Array (Realtek Audio)'),
    dev('communications', 'Communications - Microphone Array (Realtek Audio)'),
    dev('abc123', 'Microphone Array (Realtek Audio)'),
    dev('usb9', 'USB Microphone (Blue Yeti)'),
    dev('cam1', 'Camera mic', 'videoinput'),
    dev('nolabel', ''),
  ];
  for (const [wanted, want] of [
    ['Microphone Array (Realtek Audio)', 'abc123'],
    ['microphone array (realtek audio)', 'abc123'],
    ['USB Microphone', 'usb9'],
    ['Blue Yeti', 'usb9'],
    ['Default - USB Microphone (Blue Yeti)', 'usb9'],
    ['Some Headset', null],
    ['mic', null],
    ['', null],
    [null, null],
  ] as Array<[string | null, string | null]>) {
    assert.equal(pickMic(devices, wanted), want, String(wanted));
  }
  assert.equal(pickMic([dev('default', 'Default - Headset (Jabra)')], 'Headset (Jabra)'), 'default', 'the alias is better than nothing');

  // Words go to the caret, with a space first only when it follows a word.
  for (const [draft, caret, said, want] of [
    ['', 0, 'hello there', 'hello there'],
    ['Fix the bug', 11, 'in the parser', ' in the parser'],
    ['Fix the bug ', 12, 'in the parser', 'in the parser'],
    ['Fix the bug\n', 12, 'then test', 'then test'],
    ['Fix the bug', 11, '. Thanks. ', '. Thanks.'],
    ['Fix the bug', 11, ', please', ', please'],
    ['Fix the bug and test', 7, 'really', ' really'],
    ['Fix the', null, 'bug', ' bug'],
    ['', null, '  spaced out  ', 'spaced out'],
    ['anything', 3, '   ', ''],
  ] as Array<[string, number | null, string, string]>) {
    assert.equal(spokenInsert(draft, caret, said), want, `${JSON.stringify(draft)} @ ${caret} + ${JSON.stringify(said)}`);
  }

  // What the pill says for each failure, and what it offers.
  for (const [error, message, retry, settings] of [
    [new MicError(), NO_MIC, false, true],
    [new ApiError('not_set_up', "Dictation isn't set up yet."), NOT_SET_UP, false, true],
    [new ApiError('bad_address', 'That address is on your network.'), 'That address is on your network.', false, true],
    [new ApiError('unreachable', "Speech-to-text didn't answer."), NO_ANSWER, true, false],
    [new ApiError('server', 'Speech-to-text answered with an error (HTTP 500).'), NO_ANSWER, true, false],
    [new ApiError('transport', 'closed'), NO_ANSWER, true, false],
    [new Error('anything'), NO_ANSWER, true, false],
  ] as Array<[unknown, string, boolean, boolean]>) {
    const p = problemOf(error);
    assert.deepEqual([p.message, p.retry, p.settings], [message, retry, settings]);
  }
  for (const code of ['too_big', 'bad_audio']) {
    const p = problemOf(new ApiError(code, 'x'));
    assert.deepEqual([p.retry, p.settings, p.message.length > 10], [false, false, true], code);
  }
});
