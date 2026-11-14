import assert from 'node:assert/strict';
import { test } from 'node:test';
import { gpuFakeOps } from '../dev/fake-gpu.ts';
import { gpuOps, type LockView } from './api.ts';
import { clockTime, comeBackSentence, gpuControl, lockSentence, unloadConfirmLines } from './model.ts';

const lock = (p: Partial<LockView> = {}): LockView => ({ state: 'on', working: false, report: [], anyLocal: true, liftMinutes: 5, ...p });

test('the control: which face shows, what it says and when it is hidden', () => {
  const at = new Date(2026, 10, 13, 21, 4).getTime();
  const rows: [string, LockView | null, Parameters<typeof gpuControl>[1], Partial<ReturnType<typeof gpuControl>>][] = [
    ['not read yet', null, 'wide', { face: 'none' }],
    ['unlocked, a server on', lock(), 'wide', { face: 'unload', label: 'Unload local models' }],
    ['unlocked, a server on, mid bar', lock(), 'mid', { face: 'unload' }],
    ['unlocked, narrow bar', lock(), 'narrow', { face: 'none' }],
    ['unlocked, tight bar', lock(), 'tight', { face: 'none' }],
    ['unlocked, no server on', lock({ anyLocal: false }), 'wide', { face: 'none' }],
    ['unloaded by the user', lock({ state: 'manual', since: at }), 'wide', { face: 'locked', label: 'GPU reserved', lock: 'manual', tooltip: 'Local models are unloaded since 21:04', working: false }],
    ['the pill stays in the tightest bar', lock({ state: 'manual', since: at }), 'tight', { face: 'locked', lock: 'manual' }],
    ['the pill shows with no server on', lock({ state: 'manual', since: at, anyLocal: false }), 'wide', { face: 'locked' }],
    ['held by a game', lock({ state: 'game', game: 'game.exe', since: at }), 'narrow', { face: 'locked', lock: 'game', tooltip: 'Local models are unloaded while game.exe runs' }],
    ['unloading', lock({ state: 'manual', since: at, working: true }), 'wide', { face: 'locked', label: 'Unloading…', working: true }],
    ['turning back on: no popover to open', lock({ working: true }), 'wide', { face: 'locked', label: 'Turning local models back on…', working: true, lock: undefined, keepOff: undefined }],
    ['a game lock offers to keep models off', lock({ state: 'game', game: 'game.exe', since: at }), 'wide', { keepOff: true }],
    ['a manual lock does not', lock({ state: 'manual', since: at }), 'wide', { keepOff: false }],
  ];
  for (const [name, view, tier, want] of rows) {
    const got = gpuControl(view, tier);
    for (const [k, v] of Object.entries(want)) assert.equal((got as unknown as Record<string, unknown>)[k], v, `${name}: ${k}`);
  }
  assert.equal(clockTime(new Date(2026, 0, 2, 7, 5).getTime()), '07:05');
  assert.equal(lockSentence(lock({ state: 'game' })), 'Local models are unloaded while a game runs');
  assert.equal(comeBackSentence({ state: 'game', liftMinutes: 5 }), 'They come back 5 minutes after it closes, or turn them back on here.');
  assert.match(comeBackSentence({ state: 'game', liftMinutes: 1 }), /1 minute after/);
  assert.match(comeBackSentence({ state: 'manual', liftMinutes: 5 }), /until you turn them back on/);
  // The confirm names the restart task only when one is set.
  assert.deepEqual(unloadConfirmLines(), ['This stops all running local-model chats. Unload?', 'Strata and Ollama unload their models and nothing loads a model until you turn them back on.']);
  assert.equal(unloadConfirmLines('Model Restart')[1], 'Strata and Ollama unload their models, the restart task "Model Restart" is paused and nothing loads a model until you turn them back on.');
});

test('the dev host: unload answers at once and finishes later, a second unload is refused, turning back on clears it', async () => {
  const fake = gpuFakeOps({ anyLocal: () => true, liftMinutes: () => 5, restartTask: () => 'Model Restart' });
  const events: unknown[] = [];
  const api = async <T,>(op: string, args: unknown = {}): Promise<T> => fake.handler(op, args as Record<string, unknown>, { emit: (e: unknown) => events.push(e) } as never) as T;
  const ops = gpuOps(api);
  assert.equal((await ops.state()).state, 'on');
  const started = await ops.unload();
  assert.deepEqual([started.state, started.working, started.report], ['manual', true, []]);
  await assert.rejects(ops.unload(), /already unloaded/);
  await new Promise((r) => setTimeout(r, 1400));
  const done = await ops.state();
  assert.deepEqual([done.working, done.restartTask], [false, 'Model Restart']);
  assert.equal(done.report.some((l) => l.includes('Model Restart')), true);
  assert.equal((await ops.reload()).state, 'on');
  assert.equal(events.every((e) => (e as { kind: string }).kind === 'local_lock'), true);
});
