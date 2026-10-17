import assert from 'node:assert/strict';
import { test } from 'node:test';
import { splitFrontmatter } from './frontmatter.ts';

test('scalars, quoted values, inline and block lists', () => {
  const { properties, body } = splitFrontmatter(
    '---\ntitle: "Quoted: yes"\nstatus: draft\ntags: [a, "b, c", d]\nlinks:\n  - [[X]]\n  - two\nnote: |\n  line one\n  line two\n---\nafter',
  );
  assert.equal(body, 'after');
  assert.deepEqual(properties, [
    { key: 'title', values: ['Quoted: yes'], list: false },
    { key: 'status', values: ['draft'], list: false },
    { key: 'tags', values: ['a', 'b, c', 'd'], list: true },
    { key: 'links', values: ['[[X]]', 'two'], list: true },
    { key: 'note', values: ['line one\nline two'], list: false },
  ]);
});

test('a closing "..." line works and comments are skipped', () => {
  const { properties } = splitFrontmatter('---\n# comment\na: 1\n...\nbody');
  assert.deepEqual(properties, [{ key: 'a', values: ['1'], list: false }]);
});

test('lines that are not properties are kept as value-only rows', () => {
  const { properties } = splitFrontmatter('---\na: 1\nwhat is this\n---\n');
  assert.deepEqual(properties, [
    { key: 'a', values: ['1'], list: false },
    { key: '', values: ['what is this'], list: false },
  ]);
});

test('no opener, an indented opener, or no closer means no frontmatter', () => {
  for (const t of ['a: b\n---\n', ' ---\na: b\n---\n', '---\na: b\n']) {
    assert.equal(splitFrontmatter(t).properties, null, JSON.stringify(t));
    assert.equal(splitFrontmatter(t).body, t);
  }
});

test('empty frontmatter is an empty properties list', () => {
  const r = splitFrontmatter('---\n---\nbody');
  assert.deepEqual(r.properties, []);
  assert.equal(r.body, 'body');
});
