import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RenameEdit } from './label.ts';

test('a rename edit commits once: Enter then blur sends one rename', () => {
  const sent: string[] = [];
  const e = new RenameEdit((t) => sent.push(t));
  e.commit('New title');
  e.commit('New title');
  assert.deepEqual(sent, ['New title']);
});

test('Escape cancels: the blur fired while the input unmounts does not commit', () => {
  const sent: string[] = [];
  const e = new RenameEdit((t) => sent.push(t));
  e.cancel();
  e.commit('typed then escaped');
  assert.deepEqual(sent, []);
});

test('a blur alone commits', () => {
  const sent: string[] = [];
  new RenameEdit((t) => sent.push(t)).commit('kept');
  assert.deepEqual(sent, ['kept']);
});
