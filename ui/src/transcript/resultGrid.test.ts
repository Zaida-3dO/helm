import { test } from 'node:test';
import assert from 'node:assert/strict';
import { humanKey, parseResult } from './resultGrid.ts';

test('keys are humanised', () => {
  assert.equal(humanKey('createdAt'), 'Created at');
  assert.equal(humanKey('chat_id'), 'Chat id');
});

test('a JSON object becomes typed cells', () => {
  const g = parseResult(JSON.stringify({ enabled: true, color: '#ff8800', note: '', tags: ['a', 'b'], when: '2026-10-17T10:00:00Z', n: 3 }))!;
  const byKey = Object.fromEntries(g.map((r) => [r.key, r.cell]));
  assert.deepEqual(byKey['Enabled'], { t: 'bool', v: true });
  assert.deepEqual(byKey['Color'], { t: 'color', v: '#ff8800' });
  assert.deepEqual(byKey['Note'], { t: 'empty', v: 'Empty' });
  assert.deepEqual(byKey['Tags'], { t: 'chips', v: ['a', 'b'] });
  assert.equal(byKey['When']!.t, 'time');
  assert.deepEqual(byKey['N'], { t: 'text', v: '3' });
});

test('nesting stops at depth two and long object arrays are capped', () => {
  const g = parseResult(JSON.stringify({ a: { b: { c: { d: 1 } } }, items: Array.from({ length: 45 }, (_, i) => ({ name: `i${i}` })) }))!;
  const a = g[0]!.cell;
  assert.equal(a.t, 'group');
  const inner = a.t === 'group' ? a.v[0]!.cell : null;
  assert.equal(inner?.t, 'group');
  const deep = inner?.t === 'group' ? inner.v[0]!.cell : null;
  assert.deepEqual(deep, { t: 'text', v: '{"d":1}' });
  const items = g[1]!.cell;
  assert.equal(items.t === 'lines' && items.v.length, 40);
  assert.equal(items.t === 'lines' && items.more, 5);
});

test('anything that is not a non-empty JSON object is left as plain output', () => {
  assert.equal(parseResult('hello'), null);
  assert.equal(parseResult('{broken'), null);
  assert.equal(parseResult('[1,2]'), null);
  assert.equal(parseResult('{}'), null);
});
