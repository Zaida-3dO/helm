import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseTodos, todoSummary } from './todos.ts';
import { row } from '../dev/fixtures.ts';

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
