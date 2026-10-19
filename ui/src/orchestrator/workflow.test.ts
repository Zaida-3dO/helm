import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import { defaultSettings, fakeRun } from '../dev/fake-orchestrator.ts';
import { changeControls, changeLabel, isWorkflowConsent, needsYou, prNumber, signOffPlan, stageChips, taskMenu } from './progress.ts';
import { parseRun } from './run.ts';
import { isUnknownOp, RunStore, UNKNOWN_OP_MESSAGE } from './runstore.ts';

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
  let answer: unknown = null;
  const calls: string[] = [];
  const api = (async (op: string) => {
    calls.push(op);
    if (op === 'workflow_run_get') {
      if (answer) throw answer;
      return run;
    }
    if (op === 'consent_list') return { pending: [{ kind: 'land' }, { kind: 'test_action' }, { kind: 'allowMore' }], decided: [] };
    if (op === 'item_links_for_chats') return { links: [{ itemId: 'abc12345def', chatId: 'c1', isMain: true, linkedBy: 'user', linkedAt: 1, itemTitle: 'Login' }] };
    throw new Error(op);
  }) as never;
  const tick = () => new Promise((r) => setTimeout(r, 0));
  const asks = () => calls.filter((c) => c === 'workflow_run_get').length;
  const store = new RunStore(api, bus);
  store.ensure('c1');
  await tick();
  const e = store.entry('c1');
  assert.equal(e?.view.task?.title, 'Add the login page');
  assert.equal(e?.allowRequests, 2, 'test_action is not a workflow kind');
  assert.deepEqual(e?.item, { itemId: 'abc12345def', title: 'Login' });
  // The event re-reads.
  run = fakeRun('landing');
  bus.dispatch({ kind: 'workflow_run', chatId: 'c1' });
  await tick();
  assert.equal(store.entry('c1')?.view.changes[0]?.state, 'awaiting_merge');
  // An older host has no op: the entry goes and the store stops asking on its own.
  answer = { code: 'refused', message: UNKNOWN_OP_MESSAGE };
  store.ensure('c1');
  await tick();
  assert.equal(store.entry('c1'), undefined);
  let n = asks();
  store.ensure('c1');
  bus.dispatch({ kind: 'workflow_chat_mode', chatId: 'c1' });
  await tick();
  assert.equal(asks(), n, 'no more asks on its own after the host said it has no such op');
  // A run event means the host has the op after all: it asks again, and an answer re-enables.
  answer = null;
  bus.dispatch({ kind: 'workflow_run', chatId: 'c1' });
  await tick();
  assert.equal(asks(), n + 1);
  assert.equal(store.entry('c1')?.view.changes[0]?.state, 'awaiting_merge');
  n = asks();
  store.ensure('c1');
  await tick();
  assert.equal(asks(), n + 1, 'asking on its own again');
});

test('a refusal from the op itself is scoped to that call', async () => {
  const bus = new EventBus();
  let answer: unknown = null;
  let asks = 0;
  const api = (async (op: string) => {
    if (op === 'workflow_run_get') {
      asks++;
      if (answer) throw answer;
      return fakeRun('review');
    }
    if (op === 'consent_list') return { pending: [], decided: [] };
    if (op === 'item_links_for_chats') return { links: [] };
    throw new Error(op);
  }) as never;
  const tick = () => new Promise((r) => setTimeout(r, 0));
  const store = new RunStore(api, bus);
  store.ensure('c1');
  await tick();
  for (const refusal of [
    { code: 'refused', message: 'Orchestrator is off for this chat.' },
    { code: 'conflict', message: 'busy' },
    { code: 'internal', message: 'database is locked' },
  ]) {
    answer = refusal;
    store.ensure('c1');
    await tick();
    assert.equal(store.entry('c1')?.view.task?.title, 'Add the login page', `${refusal.code} keeps what is shown`);
  }
  answer = null;
  const before = asks;
  store.ensure('c1');
  await tick();
  assert.equal(asks, before + 1, 'the next call still asks');
  // The chat is gone: its entry goes, and other chats still ask.
  answer = { code: 'not_found', message: 'No such chat.' };
  store.ensure('c1');
  await tick();
  assert.equal(store.entry('c1'), undefined);
  answer = null;
  store.ensure('c2');
  await tick();
  assert.ok(store.entry('c2'));
  assert.ok(isUnknownOp({ code: 'refused', message: UNKNOWN_OP_MESSAGE }));
  assert.ok(!isUnknownOp({ code: 'refused', message: 'Something else.' }));
  assert.ok(!isUnknownOp({ code: 'bad_args', message: UNKNOWN_OP_MESSAGE }));
});

test('each change shows the buttons for what awaits the user, and acts on the names the host gave', () => {
  const ids = (kind: string) => parseRun(fakeRun(kind)).changes.map((c) => changeControls(c).map((b) => b.action).join(','));
  assert.deepEqual(ids('landing'), ['merge,decline,drop', '']);
  assert.deepEqual(ids('failed'), ['retry,drop']);
  assert.deepEqual(ids('review'), ['']);
  const handed = parseRun({ ...fakeRun('landing'), changes: [{ repo: 'github.com/o/r', branch: 'b', state: 'handed_off' }] });
  assert.deepEqual(changeControls(handed.changes[0]!).map((b) => b.label), ['Ready for review']);
  // A branch with a hidden character is shown inert but sent back exactly as the host named it.
  const odd = parseRun({ task: { runId: 'r' }, changes: [{ repo: 'github.com/o/r', branch: 'b​', state: 'awaiting_merge' }] });
  assert.equal(odd.changes[0]?.branch, 'b⟨U+200B⟩');
  assert.deepEqual(odd.changes[0]?.target, { repo: 'github.com/o/r', branch: 'b​' });
});

test('Sign off plan is offered only for an approved plan the user still owes, with the plan id shown', () => {
  const me = { ...settings, planSignOff: 'me' as const };
  assert.equal(signOffPlan(parseRun(fakeRun('signoff')), me), 'p'.repeat(64));
  assert.equal(signOffPlan(parseRun(fakeRun('signoff')), settings), null, 'review sign-off only');
  assert.equal(signOffPlan(parseRun(fakeRun('landing')), me), null, 'already signed off');
  const unreviewed = parseRun({ ...fakeRun('signoff'), plan: { id: 'q', approved: false } });
  assert.equal(signOffPlan(unreviewed, me), null, 'plan review has not approved it');
  const noReview = { ...me, stages: { ...me.stages, planReview: { ...me.stages.planReview, use: 'off' as const } } };
  assert.equal(signOffPlan(unreviewed, noReview), 'q', 'with plan review off the plan waits only for you');
  const parked = parseRun({ ...fakeRun('signoff'), task: { ...(fakeRun('signoff').task as object), status: 'parked' } });
  assert.equal(signOffPlan(parked, me), null);
});

test('the task menu follows the task status and the settings', () => {
  const ids = (v: unknown, s = settings) => taskMenu(parseRun(v), s).map((m) => m.id);
  assert.deepEqual(ids(fakeRun('review')), ['skip', 'allowMore', 'park', 'visual', 'drop', 'end']);
  const menu = taskMenu(parseRun(fakeRun('review')), settings);
  assert.deepEqual(menu[0]?.submenu?.map((m) => m.id), ['skip:plan', 'skip:planReview', 'skip:review', 'skip:cleanup', 'skip:smoketest']);
  assert.deepEqual(menu[1]?.submenu?.[1], { id: 'allowMore:review', label: 'Allow 2 more review rounds', action: 'allowMore', fields: { stage: 'review', n: 2 } });
  assert.equal(menu[4]?.submenu?.[0]?.change?.target.branch, 'feat/login');
  const off = { ...settings, stages: { ...settings.stages, cleanup: { ...settings.stages.cleanup, use: 'off' as const } } };
  assert.ok(!taskMenu(parseRun(fakeRun('review')), off)[0]?.submenu?.some((m) => m.id === 'skip:cleanup'));
  const task = fakeRun('review').task as Record<string, unknown>;
  assert.deepEqual(ids({ ...fakeRun('review'), task: { ...task, status: 'parked', visual: true } }), ['reopen', 'notVisual', 'drop', 'end']);
  assert.deepEqual(ids({ ...fakeRun('landing'), task: { ...task, status: 'handed_off' } }), ['reopen', 'drop', 'end']);
  assert.deepEqual(ids(fakeRun('none')), []);
});
