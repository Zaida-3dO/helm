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
});
