import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import { defaultSettings, fakeRun } from '../dev/fake-orchestrator.ts';
import { changeLabel, isWorkflowConsent, needsYou, prNumber, stageChips } from './progress.ts';
import { parseRun } from './run.ts';
import { RunStore } from './runstore.ts';

const settings = defaultSettings(true);
const states = (kind: string, s = settings) => stageChips(parseRun(fakeRun(kind)), s)?.map((c) => `${c.id}:${c.state}`).join(' ');

test('no task gives no stages', () => {
  assert.equal(stageChips(parseRun(fakeRun('none')), settings), null);
});

test('stages follow the task: plan, sign-off wait, review round, landing', () => {
  assert.equal(states('plan'), 'plan:current planReview:pending build:pending review:pending cleanup:pending land:pending');
  assert.equal(states('review'), 'plan:done planReview:done build:done review:current cleanup:pending land:pending');
  assert.equal(states('landing'), 'plan:done planReview:done build:done review:done cleanup:done land:current');
});

test('a plan sign-off the user owes holds plan review open and is a Needs you item', () => {
  const me = { ...settings, planSignOff: 'me' as const };
  const view = parseRun(fakeRun('signoff'));
  const chip = stageChips(view, me)?.find((c) => c.id === 'planReview');
  assert.deepEqual([chip?.state, chip?.detail], ['current', 'needs your sign-off']);
  assert.deepEqual(needsYou(view, me, 0).map((n) => n.text), ['plan sign-off']);
  // When the user's sign-off is not required, the same plan is simply approved.
  assert.deepEqual(needsYou(view, settings, 0), []);
  assert.equal(stageChips(view, settings)?.find((c) => c.id === 'planReview')?.state, 'done');
});

test('a stage that is off in the settings, or skipped for this task, shows as off', () => {
  const off = { ...settings, stages: { ...settings.stages, cleanup: { ...settings.stages.cleanup, use: 'off' as const } } };
  assert.match(states('review', off) ?? '', /cleanup:off/);
  const view = parseRun({ ...fakeRun('review'), task: { ...(fakeRun('review').task as object), skipped: ['review'] } });
  assert.equal(stageChips(view, settings)?.find((c) => c.id === 'review')?.state, 'off');
});

test('the review round shown is the next one, and a spent cap needs you', () => {
  const view = parseRun(fakeRun('review'));
  assert.equal(stageChips(view, settings)?.find((c) => c.id === 'review')?.detail, 'round 2 of 3');
  const capped = parseRun(fakeRun('cap'));
  assert.deepEqual(needsYou(capped, settings, 0).map((n) => n.text), ['review cap · app feat/login']);
});

test('land states need you where the user acts, and workflow Allow requests count', () => {
  assert.deepEqual(needsYou(parseRun(fakeRun('landing')), settings, 0).map((n) => n.text), ['merge · app feat/login']);
  assert.deepEqual(needsYou(parseRun(fakeRun('failed')), settings, 2).map((n) => n.text), ['land failed · app feat/login', '2 allow requests']);
  assert.equal(changeLabel('awaiting_merge'), 'Awaiting your Merge');
  assert.equal(changeLabel('declined'), 'Merge declined');
  assert.ok(isWorkflowConsent('land') && !isWorkflowConsent('test_action'));
  assert.equal(prNumber('https://github.com/example/app/pull/12'), '#12');
  assert.equal(prNumber('https://example.com/x'), null);
});

test('agent text is made inert and a missing field does not throw', () => {
  const v = parseRun({ task: { title: 'Do it‮evil', runId: 'r' }, changes: [{ branch: 'b​', missing: ['x‮y'] }] });
  assert.equal(v.task?.title, 'Do it⟨U+202E⟩evil');
  assert.equal(v.changes[0]?.branch, 'b⟨U+200B⟩');
  assert.equal(v.changes[0]?.missing[0], 'x⟨U+202E⟩y');
  assert.deepEqual(parseRun(null), { task: null, plan: null, changes: [], otherTasks: [] });
});

test('the run store reads the run, counts only workflow Allow requests, and goes quiet when the host has no op', async () => {
  const bus = new EventBus();
  let run: unknown = fakeRun('review');
  let refuse = false;
  const calls: string[] = [];
  const api = (async (op: string) => {
    calls.push(op);
    if (op === 'workflow_run_get') {
      if (refuse) throw { code: 'refused', message: 'This operation is not available here.' };
      return run;
    }
    if (op === 'consent_list') return { pending: [{ kind: 'land' }, { kind: 'test_action' }, { kind: 'allowMore' }], decided: [] };
    if (op === 'item_links_for_chats') return { links: [{ itemId: 'abc12345def', chatId: 'c1', isMain: true, linkedBy: 'user', linkedAt: 1, itemTitle: 'Login' }] };
    throw new Error(op);
  }) as never;
  const store = new RunStore(api, bus);
  store.ensure('c1');
  await new Promise((r) => setTimeout(r, 0));
  const e = store.entry('c1');
  assert.equal(e?.view.task?.title, 'Add the login page');
  assert.equal(e?.allowRequests, 2, 'test_action is not a workflow kind');
  assert.deepEqual(e?.item, { itemId: 'abc12345def', title: 'Login' });
  // The event re-reads.
  run = fakeRun('landing');
  bus.dispatch({ kind: 'workflow_run', chatId: 'c1' });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.entry('c1')?.view.changes[0]?.state, 'awaiting_merge');
  // An older host has no op: the entry goes and the store stops asking.
  refuse = true;
  bus.dispatch({ kind: 'workflow_run', chatId: 'c1' });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.entry('c1'), undefined);
  const n = calls.filter((c) => c === 'workflow_run_get').length;
  bus.dispatch({ kind: 'workflow_run', chatId: 'c1' });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(calls.filter((c) => c === 'workflow_run_get').length, n, 'no more asks after the host said it has no such op');
});
