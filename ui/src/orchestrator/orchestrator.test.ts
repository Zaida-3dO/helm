import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import type { ChatModeView, WorkflowSettings } from './api.ts';
import { assemble, textsOf } from './draft.ts';
import { listFromText, rulesFromText, rulesToText, switchModel } from './model.ts';
import { OrchestratorStore } from './store.ts';
import { defaultSettings } from '../dev/fake-orchestrator.ts';

const view = (over: Partial<ChatModeView> = {}): ChatModeView => ({ chatId: 'c1', mode: 'off', setBy: 'user', inEffect: false, running: false, ...over });

test('off and auto switches turn on when clicked; on turns off', () => {
  assert.deepEqual(
    [switchModel(view({ mode: 'off' }), false), switchModel(view({ mode: 'auto', setBy: 'default' }), false), switchModel(view({ mode: 'on', inEffect: true }), false)].map((m) => [m.look, m.next]),
    [['off', 'on'], ['auto', 'on'], ['on-user', 'off']],
  );
  assert.match(switchModel(view({ mode: 'auto' }), false).tooltip, /The agent may turn it on for a task/);
});

test('an agent-made switch-on shows its reason, and the reason is the tooltip', () => {
  const m = switchModel(view({ mode: 'on', setBy: 'agent', reason: 'multi-step change', inEffect: true }), false);
  assert.equal(m.look, 'on-agent');
  assert.equal(m.tooltip, 'Orchestrator: on. Turned on by the agent: multi-step change. Click to turn it off.');
});

test('a running turn makes the on switch inert and says when it can be turned off', () => {
  for (const [v, busy] of [[view({ mode: 'on', running: true }), false], [view({ mode: 'on' }), true]] as const) {
    const m = switchModel(v, busy);
    assert.deepEqual([m.clickable, m.next, m.dimmed], [false, null, true]);
    assert.match(m.tooltip, /turn it off when this turn ends/);
  }
  // Turning on is fine mid-turn: it lands as a note.
  assert.equal(switchModel(view({ mode: 'off' }), true).next, 'on');
});

test('an unavailable switch is inert and says why', () => {
  const m = switchModel(view({ unavailable: 'This chat is archived.' }), false);
  assert.deepEqual([m.look, m.clickable, m.next, m.tooltip], ['unavailable', false, null, 'This chat is archived.']);
});

test('lists split on commas and newlines and drop repeats', () => {
  assert.deepEqual(listFromText('a, b\nc ,, a'), ['a', 'b', 'c']);
});

test('repository rules: parsed, printed back, and a draft rule with a method is refused', () => {
  const r = rulesFromText('github.com/o/a merge squash\n\ngithub.com/o/b draft');
  assert.deepEqual(r, { rules: [{ repo: 'github.com/o/a', mode: 'merge', method: 'squash' }, { repo: 'github.com/o/b', mode: 'draft' }] });
  assert.equal(rulesToText('rules' in r ? r.rules : []), 'github.com/o/a merge squash\ngithub.com/o/b draft');
  assert.ok('error' in rulesFromText('github.com/o/b draft squash'));
  assert.ok('error' in rulesFromText('github.com/o/b'));
  assert.ok('error' in rulesFromText('github.com/o/b merge fast'));
  assert.deepEqual(rulesFromText('github.com/o/c merge fast_forward'), { rules: [{ repo: 'github.com/o/c', mode: 'merge', method: 'fast_forward' }] });
});

test('assemble round-trips untouched settings and applies edited texts', () => {
  const s: WorkflowSettings = { ...defaultSettings(true), homeRepos: ['github.com/o/notes'] };
  const t = textsOf(s);
  const same = assemble(s, t);
  assert.ok('settings' in same);
  assert.deepEqual('settings' in same ? same.settings : null, s);
  const edited = assemble(s, { ...t, flipLabel: ' ready ', protectedBranches: 'main, develop', rules: 'github.com/o/a merge' });
  assert.ok('settings' in edited);
  if ('settings' in edited) {
    assert.equal(edited.settings.landing.flipLabel, 'ready');
    assert.deepEqual(edited.settings.landing.protectedBranches, ['main', 'develop']);
    assert.deepEqual(edited.settings.landing.repos, [{ repo: 'github.com/o/a', mode: 'merge' }]);
  }
  const blank = assemble(s, { ...t, flipLabel: '  ' });
  assert.ok('settings' in blank && blank.settings.landing.flipLabel === null);
});

test('the store re-reads a chat on its event, and shows a refusal word for word', async () => {
  const bus = new EventBus();
  let mode: ChatModeView['mode'] = 'off';
  let refuse: string | null = null;
  const calls: string[] = [];
  const api = (async (op: string, args: Record<string, unknown>) => {
    calls.push(op);
    if (op === 'workflow_settings_get') return defaultSettings(true);
    if (op === 'workflow_chat_mode_get') return view({ chatId: String(args.chatId), mode });
    if (op === 'workflow_chat_mode_set') {
      if (refuse) throw { code: 'conflict', message: refuse };
      mode = args.mode as ChatModeView['mode'];
      return { view: view({ mode }), row: 'Orchestrator turned on.' };
    }
    throw new Error(op);
  }) as never;
  const store = new OrchestratorStore({ api, bus });
  store.ensure('c1');
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.view('c1')?.mode, 'off');
  assert.equal(store.settings?.enabled, true);
  mode = 'on';
  bus.dispatch({ kind: 'workflow_chat_mode', chatId: 'c1' });
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.view('c1')?.mode, 'on');
  // An event for a chat nobody asked about is not fetched.
  const before = calls.length;
  bus.dispatch({ kind: 'workflow_chat_mode', chatId: 'other' });
  assert.equal(calls.length, before);
  refuse = 'A Claude Code chat is running a turn; turn Orchestrator on or off once it has finished.';
  assert.equal(await store.set('c1', 'off'), undefined);
  assert.equal(store.error('c1'), refuse);
  assert.equal(store.view('c1')?.mode, 'on', 'a refused click changes nothing');
  refuse = null;
  assert.equal(await store.set('c1', 'on'), 'Orchestrator turned on.');
  assert.equal(store.error('c1'), undefined);
});
