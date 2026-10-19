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
