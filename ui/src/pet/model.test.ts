import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CELEBRATE_MS,
  FAILED_MS,
  FailureWatch,
  SLEEP_AFTER_MS,
  bubbleOf,
  clickTarget,
  initialState,
  nextPoseChange,
  parsePet,
  poseOf,
  throttled,
  withCompletion,
  withFailure,
  withSummary,
  type PetState,
} from './model.ts';

const T0 = 1_000_000;
const row = (chatId: string) => ({ chatId, title: chatId, project: 'p', kind: 'question', reason: '' });
const summary = (needsYou: string[], running: string[], unread: string[]) => ({
  needsYou: needsYou.map(row),
  running: running.map(row),
  unread: unread.map(row),
});
const state = (over: Partial<PetState>): PetState => ({ ...initialState(T0), ...over });

test('the pose follows the priority list and the bubble says what needs doing', () => {
  const cases: { name: string; s: PetState; at: number; pose: string; bubble: string | null }[] = [
    { name: 'nothing happening', s: state({}), at: T0 + 1000, pose: 'idle', bubble: null },
    { name: 'one chat working', s: state({ running: 1 }), at: T0, pose: 'working', bubble: null },
    { name: 'a question beats everything', s: state({ needsYou: 1, running: 2, completedAt: T0, failedAt: T0 }), at: T0 + 1, pose: 'waiting', bubble: '1 needs you' },
    { name: 'several questions', s: state({ needsYou: 2 }), at: T0, pose: 'waiting', bubble: '2 need you' },
    { name: 'a crowd of questions is capped', s: state({ needsYou: 250 }), at: T0, pose: 'waiting', bubble: '99+ need you' },
    { name: 'a finish celebrates for six seconds', s: state({ completedAt: T0, unread: 3, running: 1 }), at: T0 + CELEBRATE_MS - 1, pose: 'celebrate', bubble: '3 done' },
    { name: 'then back to work', s: state({ completedAt: T0, running: 1 }), at: T0 + CELEBRATE_MS, pose: 'working', bubble: null },
    { name: 'a finish beats a failure', s: state({ completedAt: T0, failedAt: T0 }), at: T0 + 10, pose: 'celebrate', bubble: null },
    { name: 'a failure shows for six seconds', s: state({ failedAt: T0, running: 1 }), at: T0 + FAILED_MS - 1, pose: 'failed', bubble: null },
    { name: 'then it is over', s: state({ failedAt: T0 }), at: T0 + FAILED_MS, pose: 'idle', bubble: null },
    { name: 'unread replies are counted', s: state({ unread: 120 }), at: T0, pose: 'idle', bubble: '99+ done' },
    { name: 'ten quiet minutes is sleep', s: state({}), at: T0 + SLEEP_AFTER_MS, pose: 'sleeping', bubble: null },
    { name: 'running work keeps it awake', s: state({ running: 1 }), at: T0 + SLEEP_AFTER_MS * 2, pose: 'working', bubble: null },
  ];
  for (const c of cases) {
    assert.equal(poseOf(c.s, c.at), c.pose, c.name);
    assert.equal(bubbleOf(c.s), c.bubble, c.name);
    if (c.bubble) assert.ok(c.bubble.length <= 18, c.name);
  }
});

test('events move the state, and the next change is the nearest timer', () => {
  let s = initialState(T0);
  s = withSummary(s, summary(['a'], ['b', 'c'], ['d']), T0 + 5);
  assert.deepEqual([s.needsYou, s.running, s.unread, s.lastEventAt], [1, 2, 1, T0 + 5]);
  // The same counts again are not an event (the pet may sleep through a repeated summary).
  assert.equal(withSummary(s, summary(['a'], ['b', 'c'], ['d']), T0 + 99).lastEventAt, T0 + 5);
  // A summary that is not one reads as empty.
  assert.deepEqual([withSummary(s, null, T0 + 6).needsYou, withSummary(s, { needsYou: 'x' }, T0 + 6).running], [0, 0]);
  s = withCompletion(s, T0 + 100);
  assert.equal(s.completedAt, T0 + 100);
  s = withFailure(s, T0 + 200);
  assert.equal(s.failedAt, T0 + 200);
  assert.equal(nextPoseChange(s, T0 + 300), T0 + 100 + CELEBRATE_MS);
  assert.equal(nextPoseChange(s, T0 + 100 + CELEBRATE_MS), T0 + 200 + FAILED_MS);
  assert.equal(nextPoseChange(initialState(T0), T0), T0 + SLEEP_AFTER_MS);
  assert.equal(nextPoseChange(initialState(T0), T0 + SLEEP_AFTER_MS), null);
});

test('a failure is a working chat that ends interrupted, and a click goes where it is needed', () => {
  const watch = new FailureWatch();
  const chat = (id: string, status: string, over: object = {}) => ({ id, status, muted: false, archived: false, ...over });
  const steps: [unknown, boolean, string][] = [
    [chat('a', 'running'), false, 'working is not a failure'],
    [chat('a', 'interrupted'), true, 'running then interrupted'],
    [chat('a', 'interrupted'), false, 'the same record again'],
    [chat('b', 'interrupted'), false, 'first heard of already interrupted'],
    [chat('c', 'stopping'), false, 'stopping'],
    [chat('c', 'stopped'), false, 'a clean stop'],
    [chat('d', 'running'), false, 'working'],
    [chat('d', 'interrupted', { muted: true }), false, 'muted'],
    [chat('e', 'starting'), false, 'starting'],
    [chat('e', 'interrupted', { archived: true }), false, 'archived'],
    [null, false, 'not a record'],
  ];
  for (const [record, want, name] of steps) assert.equal(watch.chat(record), want, name);
  // A chat the summary listed as running counts even though no earlier record was seen.
  watch.setRunning(summary([], ['f'], []));
  assert.equal(watch.chat(chat('f', 'interrupted')), true);

  assert.equal(clickTarget(summary(['q1', 'q2'], [], ['u1'])), 'q1');
  assert.equal(clickTarget(summary([], ['r'], ['u1', 'u2'])), 'u1');
  assert.equal(clickTarget(summary([], ['r'], [])), null);
  assert.equal(clickTarget(undefined), null);
});

test('the stored preference reads leniently, and a drag reports the spot it stopped at', () => {
  assert.deepEqual(parsePet(undefined), { enabled: false, kind: 'puffin', size: 'm', bubble: true, position: null });
  assert.deepEqual(parsePet({ enabled: true, kind: 'crab', size: 'l', bubble: false, position: { x: -3, y: 40 } }), {
    enabled: true,
    kind: 'crab',
    size: 'l',
    bubble: false,
    position: { x: -3, y: 40 },
  });
  const odd = parsePet({ enabled: 'yes', kind: 'gull', size: 'xl', bubble: 1, position: { x: 1.5, y: 2 } });
  assert.deepEqual(odd, { enabled: false, kind: 'puffin', size: 'm', bubble: true, position: null });

  let now = 0;
  const hold: { timer: (() => void) | null } = { timer: null };
  const seen: number[] = [];
  const t = throttled((n: number) => seen.push(n), 350, {
    set: (h) => {
      hold.timer = h;
      return 1;
    },
    clear: () => {
      hold.timer = null;
    },
    now: () => now,
  });
  t.call(1);
  now = 100;
  t.call(2);
  now = 200;
  t.call(3);
  assert.deepEqual(seen, [1], 'the first goes at once, the rest wait');
  now = 350;
  hold.timer?.();
  assert.deepEqual(seen, [1, 3], 'the wait ends with the latest');
  now = 400;
  t.call(4);
  t.cancel();
  hold.timer?.();
  assert.deepEqual(seen, [1, 3], 'a cancelled call never fires');
});
