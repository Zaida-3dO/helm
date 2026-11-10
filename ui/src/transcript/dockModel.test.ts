import assert from 'node:assert/strict';
import { test } from 'node:test';
import { backgroundShells, dockShown } from './dockModel.ts';

const shell = { id: 's', title: 'npm run dev', command: 'npm run dev', kind: 'shell' as const, startedAt: 1 };
const agent = { id: 'a', title: 'Survey', command: null, kind: 'agent' as const, startedAt: 1 };

test('background agents are not counted twice: the chip lists shells, the roster counts agents', () => {
  assert.deepEqual(backgroundShells({ backgroundTasks: [shell, agent] }), [shell]);
  assert.deepEqual(backgroundShells({ backgroundTasks: [agent] }), []);
});

test('the dock stays up for a running turn, a background shell or a working subagent', () => {
  assert.equal(dockShown(false, { backgroundTasks: [agent], subagentsRunning: 0 }), false);
  assert.equal(dockShown(false, { backgroundTasks: [agent], subagentsRunning: 1 }), true);
  assert.equal(dockShown(false, { backgroundTasks: [shell], subagentsRunning: 0 }), true);
  assert.equal(dockShown(true, { backgroundTasks: [], subagentsRunning: 0 }), true);
});

test("line 1 names what the chat is doing", async () => {
  const { dockState } = await import('./dockModel.ts');
  const t = { compacting: null, interactions: [], liveTools: [], subagentsRunning: 0 };
  assert.equal(dockState('starting', t).label, 'Starting agent');
  assert.equal(dockState('stopping', t).label, 'Stopping safely');
  assert.equal(dockState('running', t).label, 'Working');
  assert.equal(dockState('running', { ...t, subagentsRunning: 2 }).detail, '2 subagents working');
  assert.equal(dockState('running', { ...t, interactions: [{} as never] }).label, 'Waiting for you');
  assert.equal(dockState('running', { ...t, compacting: { retrying: true } as never }).label, 'Compacting conversation');
  assert.equal(dockState('idle', { ...t, subagentsRunning: 1 }).key, 'subagents');
  assert.equal(dockState('idle', t).key, 'background');
  // A local model being loaded takes line 1 until the host clears it, and only while the turn is starting or running.
  const loading = { text: 'Loading qwen3:8b…', startedAt: 1 };
  assert.deepEqual([dockState('starting', { ...t, loading }).key, dockState('starting', { ...t, loading }).label], ['loading', 'Loading qwen3:8b…']);
  assert.equal(dockState('running', { ...t, loading }).key, 'loading');
  assert.equal(dockState('stopping', { ...t, loading }).key, 'stopping');
  assert.equal(dockState('running', { ...t, loading: undefined }).label, 'Working');
  // A local chat's line speaks in halves of the wait: standing at the model server, or being
  // answered by it now. The server answers one request at a time.
  const local = { model: 'local/strata/qwen', localAhead: 1 };
  assert.deepEqual([dockState('running', t, local).key, dockState('running', t, local).label], ['model-waiting', 'Waiting for the local model (1 ahead)']);
  assert.equal(dockState('running', t, { ...local, localAhead: 0 }).label, 'Waiting for the local model');
  assert.equal(dockState('running', t, { ...local, localAhead: null }).label, 'Using the local model');
  // A Claude chat is none of this; a card that needs you and a loading line still come first.
  assert.equal(dockState('running', t, { model: 'opus', localAhead: 2 }).label, 'Working');
  assert.equal(dockState('running', { ...t, interactions: [{} as never] }, local).label, 'Waiting for you');
  assert.equal(dockState('running', { ...t, loading }, local).key, 'loading');
});

test('the settled line comes from the last turn when it ended with a reply', async () => {
  const { finishedTurn, finishedCount } = await import('./dockModel.ts');
  const { row } = await import('../dev/fixtures.ts');
  const user = row({ id: 1, role: 'user', timestamp: 0 });
  const note = row({ id: 2, role: 'assistant', text: 'looking', timestamp: 1000 });
  const read = row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 });
  const edit = row({ id: 4, role: 'toolResult', toolName: 'Edit', timestamp: 3000 });
  const reply = row({ id: 5, role: 'assistant', text: 'done', timestamp: 48_000 });
  const cases: [string, ReturnType<typeof row>[], string | null, number | null][] = [
    ['tools and a reply', [user, note, read, reply], '1 done', 47_000],
    ['edits are counted as files', [user, note, read, edit, reply], '1 file edited', 47_000],
    ['a quick reply is timed from the message', [user, row({ id: 2, role: 'assistant', text: 'Hi', timestamp: 500 })], null, 500],
    ['one file edited twice is one file', [user, note, { ...edit, details: { toolArgs: JSON.stringify({ file_path: 'a.ts' }) } }, { ...edit, id: 6, details: { toolArgs: JSON.stringify({ file_path: 'a.ts' }) } }, reply], '1 file edited', 47_000],
    ['no reply yet', [user, read], 'none', null],
    ['a tool still running', [user, row({ id: 3, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 2000 }), reply], 'none', null],
    ['a failed turn', [user, { ...reply, status: 'error' as const }], 'none', null],
    ['a stopped turn', [user, note, { ...read, status: 'interrupted' as const }], 'none', null],
    ['no turn at all', [], 'none', null],
  ];
  for (const [name, rows, count, elapsed] of cases) {
    const t = finishedTurn(rows);
    assert.equal(t === null ? 'none' : finishedCount(t), count ?? null, name);
    assert.equal(t?.elapsedMs ?? null, elapsed, name);
  }
  // The settled time ends at the run's end when that is after the last row (the reply streams after its stamp).
  assert.equal(finishedTurn([user, note, read, reply], 60_000)?.elapsedMs, 59_000);
  assert.equal(finishedTurn([user, note, read, reply], 10_000)?.elapsedMs, 47_000, 'a run that ended before the last row changes nothing');
  assert.equal(finishedTurn([user, note, read, reply], null)?.elapsedMs, 47_000);
});

test('with no tool running the dock says what the turn is doing, and since when', async () => {
  const { dockActivity, lastLine } = await import('./dockModel.ts');
  const { row } = await import('../dev/fixtures.ts');
  const none = { subagents: [], subagentsRunning: 0 };
  const user = row({ id: 1, role: 'user', timestamp: 1000 });
  const sub = { toolCallId: 's1', status: 'running' as const, startedAt: 5000 } as never;
  const cases: [string, ReturnType<typeof row>[], typeof none, string, string | null, number][] = [
    ['thinking', [user, row({ id: 2, role: 'assistant', status: 'streaming', thinking: 'Checking the tokens.\n- then the dock', timestamp: 2000 })], none, 'Thinking', 'then the dock', 2000],
    ['writing', [user, row({ id: 2, role: 'assistant', status: 'streaming', thinking: 'hm', text: 'I changed the dock.', timestamp: 3000 })], none, 'Writing the reply', 'I changed the dock.', 3000],
    ['waiting on subagents', [user, row({ id: 2, role: 'toolResult', toolName: 'Agent', timestamp: 4000 })], { subagents: [sub, { ...(sub as object), toolCallId: 's2', startedAt: 4500 } as never], subagentsRunning: 2 }, 'Waiting on 2 subagents', null, 4500],
    ['after a tool', [user, row({ id: 2, role: 'toolResult', toolName: 'Read', timestamp: 6000 })], none, 'Thinking about the result', null, 6000],
    ['nothing back yet', [user], none, 'Waiting for the model', null, 700],
  ];
  for (const [name, rows, thread, label, narration, since] of cases) {
    const a = dockActivity(rows, thread, 700);
    assert.deepEqual([a.label, a.narration, a.since], [label, narration, since], name);
  }
  // A local chat the host flags as queued at the model server says so with its place in line,
  // from wherever in the turn the silence sits; without the flag the plain sentence stays.
  const queued = [user, row({ id: 2, role: 'toolResult', toolName: 'Read', timestamp: 6000 })];
  assert.deepEqual(dockActivity([user], none, 700, 2), { phase: 'model', label: 'Waiting for the local model (2 ahead)', since: 700, narration: null });
  assert.deepEqual([dockActivity(queued, none, 700, 1).label, dockActivity(queued, none, 700, 1).since], ['Waiting for the local model (1 ahead)', 6000]);
  assert.equal(dockActivity([user], none, 700, 0).label, 'Waiting for the local model');
  assert.equal(dockActivity(queued, none, 700, null).label, 'Thinking about the result');
  assert.equal(lastLine('1. First\n\n## Second line  \n'), 'Second line');
});
