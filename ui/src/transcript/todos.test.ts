import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseTodos, todoSummary } from './todos.ts';
import { row } from '../dev/fixtures.ts';
import { newestTurn, stepsDone, tickedIndexes, turnChecklist } from './checklist.ts';

const args = {
  todos: [
    { content: 'Read the docs', status: 'completed', activeForm: 'Reading the docs' },
    { content: 'Write the parser', status: 'in_progress', activeForm: 'Writing the parser' },
    { content: 'Ship it', status: 'pending', activeForm: 'Shipping it' },
  ],
};

test('a TodoWrite call reads as a checklist with its states', () => {
  const todos = parseTodos(row({ role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: JSON.stringify(args) } }));
  assert.deepEqual(todos, [
    { text: 'Read the docs', state: 'completed' },
    { text: 'Write the parser', state: 'in_progress' },
    { text: 'Ship it', state: 'pending' },
  ]);
  assert.equal(todoSummary(todos!), '1/3 done');
});

test('arguments cut short keep every whole item and drop the cut one', () => {
  const full = JSON.stringify(args);
  const cut = full.slice(0, full.indexOf('Ship it') + 3);
  const todos = parseTodos(row({ role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: cut } }));
  assert.deepEqual(
    todos?.map((t) => t.text),
    ['Read the docs', 'Write the parser'],
  );
});

test('other tools and empty lists are not checklists', () => {
  assert.equal(parseTodos(row({ role: 'toolResult', toolName: 'Bash', details: { toolArgs: JSON.stringify(args) } })), null);
  assert.equal(parseTodos(row({ role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: '{"todos":[]}' } })), null);
});

test('a turn keeps its latest checklist, from todo calls or from task calls, and ticks the steps that just finished', () => {
  const todo = (...states: string[]) => row({ role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: JSON.stringify({ todos: states.map((status, i) => ({ content: `s${i}`, status })) }) } });
  const task = (name: string, args: object, text = '') => row({ role: 'toolResult', toolName: name, text, details: { toolArgs: JSON.stringify(args) } });
  const user = row({ role: 'user', text: 'go' });
  // The newest todo call wins, and an earlier turn's list is not this turn's.
  const turn = newestTurn([todo('completed'), user, todo('pending', 'pending'), row({ role: 'toolResult', toolName: 'Bash' }), todo('completed', 'in_progress')]);
  assert.equal(turn.length, 3);
  assert.deepEqual(turnChecklist(turn)?.map((t) => t.state), ['completed', 'in_progress']);
  assert.equal(turnChecklist(newestTurn([todo('completed'), user, row({ role: 'toolResult', toolName: 'Bash' })])), null);
  // Task calls build the list one step at a time; deleted steps drop out.
  const tasks = [
    task('TaskCreate', { subject: 'Plan it' }, 'Task #1 created'),
    task('TaskCreate', { subject: 'Build it' }, 'Task #2 created'),
    task('TaskCreate', { subject: 'Drop it' }, 'Task #3 created'),
    task('TaskUpdate', { taskId: '1', status: 'completed' }),
    task('TaskUpdate', { taskId: '2', status: 'in_progress', subject: 'Build it well' }),
    task('TaskUpdate', { taskId: '3', status: 'deleted' }),
  ];
  assert.deepEqual(turnChecklist(tasks), [
    { text: 'Plan it', state: 'completed' },
    { text: 'Build it well', state: 'in_progress' },
  ]);
  assert.equal(stepsDone(turnChecklist(tasks)!), 1);
  const after = turnChecklist([...tasks, task('TaskUpdate', { taskId: '2', status: 'completed' })])!;
  assert.deepEqual([...tickedIndexes(['completed', 'in_progress'], after)], [1]);
  assert.equal(tickedIndexes(null, after).size, 0);
});
