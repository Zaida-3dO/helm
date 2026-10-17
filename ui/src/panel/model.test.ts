import assert from 'node:assert/strict';
import { test } from 'node:test';
import { APP_SET, PanelModel, SET_CAP, TOTAL_CAP } from './model.ts';
import type { CloseReason, KindRules, PanelTab } from './model.ts';

function setup(extra: Record<string, Partial<KindRules>> = {}) {
  const closes: Array<[string, CloseReason]> = [];
  const kinds: Record<string, KindRules> = {
    file: { scope: 'chat', onClose: (t, r) => closes.push([t.key, r]) },
    pr: { scope: 'app', maxTabs: 8, onClose: (t, r) => closes.push([t.key, r]) },
    pinned: { scope: 'app', evictable: false, onClose: (t, r) => closes.push([t.key, r]) },
    one: { scope: 'chat', maxTabs: 1, onClose: (t, r) => closes.push([t.key, r]) },
  };
  for (const [k, v] of Object.entries(extra)) kinds[k] = { ...(kinds[k] ?? { scope: 'chat' }), ...v };
  const m = new PanelModel((k) => kinds[k]);
  return { m, closes };
}

const keys = (tabs: PanelTab[]) => tabs.map((t) => t.key);

test('an unknown kind is refused', () => {
  const { m } = setup();
  assert.equal(m.open('nope', 'k', {}, { intent: 'user' }, 'c1'), null);
  assert.equal(m.tabs.length, 0);
});

test('reopening the same kind+key updates data; a user open selects, bumps and upgrades it', () => {
  const { m } = setup();
  const a = m.open('file', 'a', { v: 1 }, { intent: 'program' }, 'c1')!;
  assert.equal(m.counted('c1').length, 0);
  assert.equal(m.visible('c1'), false, 'program tabs never make the panel show');
  m.open('file', 'a', { v: 2 }, { intent: 'program' }, 'c1');
  assert.deepEqual(a.data, { v: 2 });
  assert.equal(a.intent, 'program');
  m.open('file', 'a', { v: 3 }, { intent: 'user' }, 'c1');
  assert.equal(m.tabs.length, 1);
  assert.equal(a.intent, 'user');
  assert.equal(m.activeEntry('c1'), a.id);
  assert.equal(m.visible('c1'), true);
});

test('chat tabs show only in their chat; app tabs in every chat', () => {
  const { m } = setup();
  m.open('file', 'a', {}, { intent: 'user' }, 'c1');
  m.open('pr', 'p', {}, { intent: 'user' }, 'c1');
  assert.deepEqual(keys(m.displayed('c1')), ['a', 'p']);
  assert.deepEqual(keys(m.displayed('c2')), ['p']);
  assert.equal(m.tabs.find((t) => t.key === 'p')!.set, APP_SET);
});

test('a chat-scoped open for another chat lands in that chat and is selected there', () => {
  const { m } = setup();
  const t = m.open('file', 'a', {}, { intent: 'user', chatId: 'c2' }, 'c1')!;
  assert.equal(t.set, 'c2');
  assert.deepEqual(keys(m.displayed('c1')), []);
  assert.equal(m.activeEntry('c2'), t.id);
});

test('the kind cap evicts the least recently used tab of that kind in the set', () => {
  const { m, closes } = setup();
  m.open('one', 'x', {}, { intent: 'user' }, 'c1');
  m.open('one', 'y', {}, { intent: 'user' }, 'c1');
  assert.deepEqual(keys(m.displayed('c1')), ['y']);
  assert.deepEqual(closes, [['x', 'evicted']]);
  // Another chat's set has its own cap.
  m.open('one', 'z', {}, { intent: 'user' }, 'c2');
  assert.deepEqual(keys(m.displayed('c1')), ['y']);
});

test('12 per set: the 13th user open evicts the least recently used tab', () => {
  const { m, closes } = setup();
  for (let i = 0; i < SET_CAP; i++) m.open('file', `f${i}`, {}, { intent: 'user' }, 'c1');
  // Touch f0 so f1 becomes the least recently used.
  m.select(m.tabs[0]!.id, 'c1');
  m.open('file', 'new', {}, { intent: 'user' }, 'c1');
  assert.equal(m.displayed('c1').length, SET_CAP);
  assert.deepEqual(closes, [['f1', 'evicted']]);
  assert.ok(keys(m.displayed('c1')).includes('f0'));
});

test('48 in total: tabs the current chat does not display are evicted first', () => {
  const { m, closes } = setup();
  for (let c = 0; c < 4; c++) for (let i = 0; i < SET_CAP; i++) m.open('file', `c${c}-${i}`, {}, { intent: 'user' }, `chat${c}`);
  assert.equal(m.tabs.length, TOTAL_CAP);
  // Make chat3's first tab the globally least recently used... it is still displayed in chat3,
  // so the eviction must pick the oldest tab of another chat instead.
  m.tabs.find((t) => t.key === 'c3-0')!.used = -1;
  m.open('pr', 'p', {}, { intent: 'user' }, 'chat3');
  assert.equal(m.tabs.length, TOTAL_CAP);
  assert.deepEqual(closes, [['c0-0', 'evicted']]);
});

test('a program open may evict only program tabs that are not shown; else it is refused, changing nothing', () => {
  const { m, closes } = setup();
  for (let i = 0; i < SET_CAP; i++) m.open('file', `u${i}`, {}, { intent: 'user' }, 'c1');
  const before = keys(m.tabs);
  assert.equal(m.open('file', 'prog', {}, { intent: 'program' }, 'c1'), null);
  assert.deepEqual(keys(m.tabs), before);
  assert.deepEqual(closes, []);

  const { m: m2, closes: c2 } = setup();
  for (let i = 0; i < SET_CAP - 1; i++) m2.open('file', `u${i}`, {}, { intent: 'user' }, 'c1');
  m2.open('file', 'p0', {}, { intent: 'program' }, 'c1');
  assert.ok(m2.open('file', 'p1', {}, { intent: 'program' }, 'c1'));
  assert.deepEqual(c2, [['p0', 'evicted']]);
});

test('a non-evictable tab refuses an open that would need its room', () => {
  const { m } = setup({ pinned: { maxTabs: 1 } });
  m.open('pinned', 'a', {}, { intent: 'user' }, 'c1');
  assert.equal(m.open('pinned', 'b', {}, { intent: 'user' }, 'c1'), null);
  assert.deepEqual(keys(m.tabs), ['a']);
});

test('onClose fires exactly once, with the reason, and never on hide', () => {
  const { m, closes } = setup();
  const a = m.open('file', 'a', {}, { intent: 'user' }, 'c1')!;
  m.hide();
  m.toggle('c1');
  assert.deepEqual(closes, []);
  assert.equal(m.close(a.id, 'user'), true);
  assert.equal(m.close(a.id, 'user'), false);
  assert.deepEqual(closes, [['a', 'user']]);
});

test('closing the selected tab selects the most recently used counted tab; program tabs never qualify', () => {
  const { m } = setup();
  const a = m.open('file', 'a', {}, { intent: 'user' }, 'c1')!;
  const b = m.open('file', 'b', {}, { intent: 'user' }, 'c1')!;
  m.open('file', 'p', {}, { intent: 'program' }, 'c1');
  const c = m.open('file', 'c', {}, { intent: 'user' }, 'c1')!;
  m.select(a.id, 'c1');
  m.select(c.id, 'c1');
  m.close(c.id, 'user');
  assert.equal(m.activeEntry('c1'), a.id);
  m.close(a.id, 'user');
  assert.equal(m.activeEntry('c1'), b.id);
  m.close(b.id, 'user');
  assert.equal(m.activeEntry('c1'), null);
  assert.equal(m.visible('c1'), false);
});

test('selecting a program tab upgrades it to user intent', () => {
  const { m } = setup();
  const p = m.open('file', 'p', {}, { intent: 'program' }, 'c1')!;
  m.select(p.id, 'c1');
  assert.equal(p.intent, 'user');
  assert.equal(m.activeEntry('c1'), p.id);
});

test('panel X hides without closing and leaves maximised; a user open un-hides', () => {
  const { m, closes } = setup({ file: { defaultMaximized: true } });
  m.open('file', 'a', {}, { intent: 'user' }, 'c1');
  assert.equal(m.maximized, true);
  m.hide();
  assert.equal(m.visible('c1'), false);
  assert.equal(m.maximized, false);
  assert.equal(m.tabs.length, 1);
  m.open('file', 'b', {}, { intent: 'user' }, 'c1');
  assert.equal(m.visible('c1'), true);
  assert.deepEqual(closes, []);
});

test('maximise is cleared when no counted tabs remain', () => {
  const { m } = setup();
  const a = m.open('file', 'a', {}, { intent: 'user' }, 'c1')!;
  m.setMaximized(true);
  m.close(a.id, 'user');
  assert.equal(m.maximized, false);
});

test('toggle: with documents it hides/shows them, without it toggles the Inspector', () => {
  const { m } = setup();
  m.toggle('c1');
  assert.equal(m.inspector, true);
  assert.equal(m.activeEntry('c1'), 'slot:changes');
  m.toggle('c1');
  assert.equal(m.inspector, false);
  m.open('file', 'a', {}, { intent: 'user' }, 'c1');
  m.toggle('c1');
  assert.equal(m.hidden, true);
  m.toggle('c1');
  assert.equal(m.hidden, false);
});

test('closing a tab falls back to a more recently used slot', () => {
  const { m } = setup();
  const a = m.open('file', 'a', {}, { intent: 'user' }, 'c1')!;
  m.setInspector(true, 'files', 'c1');
  m.select(a.id, 'c1');
  m.close(a.id, 'user');
  assert.equal(m.activeEntry('c1'), 'slot:files');
});

test('reorder moves a tab among the displayed tabs only', () => {
  const { m } = setup();
  m.open('file', 'a', {}, { intent: 'user' }, 'c1');
  m.open('file', 'x', {}, { intent: 'user' }, 'c2');
  m.open('file', 'b', {}, { intent: 'user' }, 'c1');
  const b = m.tabs.find((t) => t.key === 'b')!;
  m.reorder(b.id, 0, 'c1');
  assert.deepEqual(keys(m.displayed('c1')), ['b', 'a']);
  assert.deepEqual(keys(m.displayed('c2')), ['x']);
  m.reorder(b.id, 9, 'c1');
  assert.deepEqual(keys(m.displayed('c1')), ['a', 'b']);
});

test('dropSet closes a deleted chat’s tabs as evicted', () => {
  const { m, closes } = setup();
  m.open('file', 'a', {}, { intent: 'user' }, 'c1');
  m.open('pr', 'p', {}, { intent: 'user' }, 'c1');
  m.dropSet('c1');
  assert.deepEqual(keys(m.tabs), ['p']);
  assert.deepEqual(closes, [['a', 'evicted']]);
});

test('mounted keeps the active tab plus at most 3 keep-mounted user tabs per scope', () => {
  const { m } = setup();
  const ids = [];
  for (let i = 0; i < 6; i++) ids.push(m.open('file', `f${i}`, {}, { intent: 'user' }, 'c1')!.id);
  m.open('file', 'prog', {}, { intent: 'program' }, 'c1');
  const mounted = m.mounted('c1', () => true);
  assert.equal(mounted.length, 4);
  assert.equal(mounted[0]!.key, 'f5', 'the active tab comes first');
  assert.deepEqual(keys(mounted.slice(1)), ['f4', 'f3', 'f2']);
  assert.deepEqual(keys(m.mounted('c1', () => false)), ['f5']);
});

test('restore brings a saved tab back as program intent in its own set, never over a cap', () => {
  const { m } = setup();
  assert.equal(m.restore('', 'file', 'a', { p: 1 }), true);
  assert.equal(m.restore('c1', 'file', 'b', {}), true);
  assert.equal(m.restore('c1', 'file', 'b', {}), false, 'already open');
  assert.equal(m.restore('c1', 'pr', 'x', {}), false, 'an app kind outside the app set');
  assert.equal(m.restore('app', 'file', 'x', {}), false, 'a chat kind in the app set');
  assert.equal(m.restore('c1', 'nope', 'x', {}), false);
  assert.deepEqual(keys(m.displayed(null)), ['a']);
  const b = m.displayed('c1')[0]!;
  assert.equal(b.intent, 'program');
  assert.equal(m.visible('c1'), false, 'restored tabs never show the panel');
  for (let i = 1; i < SET_CAP; i++) assert.equal(m.restore('c1', 'file', `f${i}`, {}), true);
  assert.equal(m.restore('c1', 'file', 'over', {}), false);
  assert.equal(m.displayed('c1').length, SET_CAP);
});
