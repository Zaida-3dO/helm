import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { PanelTab } from './model.ts';
import { MAX_DATA_BYTES, MAX_ENTRIES, MAX_TOTAL_BYTES, restoreTabs, serializeTabs } from './persist.ts';
import type { Persistable } from './persist.ts';

const kinds: Record<string, Persistable> = {
  file: {
    scope: 'chat',
    persist: {
      version: 1,
      save: (t) => t.data,
      restore: (d) => (d && typeof d === 'object' && 'path' in d ? d : null),
    },
  },
  pr: { scope: 'app' },
  appdoc: { scope: 'app', persist: { version: 2, save: (t) => t.data, restore: (d) => d } },
  boom: {
    scope: 'chat',
    persist: {
      version: 1,
      save: () => {
        throw new Error('x');
      },
      restore: () => {
        throw new Error('x');
      },
    },
  },
};
const kindOf = (k: string) => kinds[k];

let n = 0;
const tab = (kind: string, key: string, set: string, data: unknown): PanelTab => ({
  id: `t${n++}`, kind, key, data, used: n, set, intent: 'user',
});

test('only kinds with persist are saved, as exactly {kind, key, v, data}, in strip order', () => {
  const value = serializeTabs(
    [tab('file', 'a', 'c1', { path: 'C:/a' }), tab('pr', 'p', 'app', {}), tab('appdoc', 'd', 'app', { x: 1 }), tab('file', 'b', 'c1', { path: 'C:/b' })],
    kindOf,
  );
  assert.deepEqual(value, {
    version: 1,
    sets: {
      c1: [
        { kind: 'file', key: 'a', v: 1, data: { path: 'C:/a' } },
        { kind: 'file', key: 'b', v: 1, data: { path: 'C:/b' } },
      ],
      app: [{ kind: 'appdoc', key: 'd', v: 2, data: { x: 1 } }],
    },
  });
});

test('serialisation stays within every limit, skipping what does not fit', () => {
  const tabs: PanelTab[] = [];
  for (let i = 0; i < 60; i++) tabs.push(tab('file', `k${i}`, `c${i % 5}`, { path: `C:/${i}` }));
  tabs.push(tab('file', 'big', 'c1', { path: 'x'.repeat(MAX_DATA_BYTES) }));
  // Exactly at the data limit fits: `{"path":"…"}` is 11 bytes of envelope.
  tabs.unshift(tab('file', 'edge', 'c0', { path: 'e'.repeat(MAX_DATA_BYTES - 11) }));
  tabs.push(tab('file', 'ctl\u0001', 'c1', { path: 'C:/x' }));
  tabs.push(tab('boom', 'b', 'c1', {}));
  const value = serializeTabs(tabs, kindOf);
  const entries = Object.values(value.sets).flat();
  assert.equal(entries.length, MAX_ENTRIES);
  assert.ok(!entries.some((e) => e.key === 'big' || e.key === 'ctl\u0001' || e.kind === 'boom'));
  assert.ok(entries.some((e) => e.key === 'edge'), 'data of exactly 4 KiB is kept');

  const wide: PanelTab[] = [];
  for (let i = 0; i < 40; i++) wide.push(tab('file', `w${i}`, 'c1', { path: 'p'.repeat(3000) }));
  const bounded = serializeTabs(wide, kindOf);
  assert.ok(new TextEncoder().encode(JSON.stringify(bounded)).length <= MAX_TOTAL_BYTES);
  assert.ok(bounded.sets.c1!.length > 0 && bounded.sets.c1!.length < 40);
});

test('restore drops unknown, unpersisted, scope-mismatched, duplicate and failing entries', () => {
  const restored = restoreTabs(
    {
      version: 1,
      sets: {
        c1: [
          { kind: 'file', key: 'a', v: 1, data: { path: 'C:/a' } },
          { kind: 'file', key: 'a', v: 1, data: { path: 'C:/a' } },
          { kind: 'file', key: 'bad', v: 1, data: { nope: 1 } },
          { kind: 'nope', key: 'n', v: 1, data: {} },
          { kind: 'pr', key: 'p', v: 1, data: {} },
          { kind: 'appdoc', key: 'wrong-set', v: 2, data: {} },
          { kind: 'boom', key: 'b', v: 1, data: {} },
          'junk',
        ],
        app: [
          { kind: 'appdoc', key: 'd', v: 2, data: { x: 1 } },
          { kind: 'file', key: 'chat-kind-in-app', v: 1, data: { path: 'C:/z' } },
        ],
        gone: [{ kind: 'file', key: 'g', v: 1, data: { path: 'C:/g' } }],
      },
    },
    kindOf,
    (id) => id !== 'gone',
  );
  assert.deepEqual(restored, [
    { set: 'c1', kind: 'file', key: 'a', data: { path: 'C:/a' } },
    { set: 'app', kind: 'appdoc', key: 'd', data: { x: 1 } },
  ]);
});

test('restore is lenient about a value that is not panelTabs v1', () => {
  for (const v of [null, 1, 'x', {}, { version: 2, sets: {} }, { version: 1, sets: 3 }]) {
    assert.deepEqual(restoreTabs(v, kindOf), []);
  }
});
