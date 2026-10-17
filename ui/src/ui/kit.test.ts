import { test } from 'node:test';
import assert from 'node:assert/strict';
import { LayerStack } from './layers.ts';
import { KeyRegistry, chordOf, normaliseChord } from './keys.ts';
import { ToastSlot } from './toasts.ts';
import { nextIndex, placeAtPoint, placeBelow } from './place.ts';
import { folderLabel, startsDrag } from '../topbar/label.ts';

const key = (k: string, mods: { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean } = {}, target?: unknown) => ({
  key: k,
  code: /^[a-z]$/i.test(k) ? `Key${k.toUpperCase()}` : undefined,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
  metaKey: !!mods.meta,
  target: target as EventTarget | null,
});

test('Escape goes menus, popovers, search, dialogs, panel, composer; newest first within a kind', () => {
  const s = new LayerStack();
  const got: string[] = [];
  const add = (kind: Parameters<LayerStack['push']>[0]['kind'], name: string) => s.push({ kind, onEscape: () => void got.push(name) });
  const offComposer = add('composer', 'composer');
  const offDialog = add('dialog', 'dialog');
  const offPanel = add('panel', 'panel');
  const offMenu1 = add('menu', 'menu1');
  const offMenu2 = add('menu', 'menu2');
  const offPop = add('popover', 'popover');
  s.escape();
  offMenu2();
  s.escape();
  offMenu1();
  s.escape();
  offPop();
  s.escape();
  offDialog();
  s.escape();
  offPanel();
  s.escape();
  offComposer();
  assert.equal(s.escape(), false, 'nothing left to take it');
  assert.deepEqual(got, ['menu2', 'menu1', 'popover', 'dialog', 'panel', 'composer']);
});

test('a search field takes Escape only while it holds text, before the dialog', () => {
  const s = new LayerStack();
  let text = 'abc';
  const got: string[] = [];
  s.push({ kind: 'dialog', onEscape: () => void got.push('dialog') });
  s.push({ kind: 'search', onEscape: () => ((text = ''), void got.push('clear')), active: () => text.length > 0 });
  s.escape();
  s.escape();
  assert.deepEqual(got, ['clear', 'dialog']);
});

test('a layer that declines passes Escape on; a busy dialog swallows it', () => {
  const s = new LayerStack();
  const got: string[] = [];
  s.push({ kind: 'composer', onEscape: () => void got.push('composer') });
  s.push({ kind: 'panel', onEscape: () => false });
  s.escape();
  const offBusy = s.push({ kind: 'dialog', onEscape: () => true });
  s.escape();
  offBusy();
  assert.deepEqual(got, ['composer']);
});

test('chords normalise; Cmd reads as Ctrl and Option as Alt', () => {
  assert.equal(normaliseChord('shift+cmd+k'), 'Ctrl+Shift+K');
  assert.equal(normaliseChord('Option+Mod+p'), 'Ctrl+Alt+P');
  assert.equal(normaliseChord('Hyper+K'), null);
  assert.equal(normaliseChord(''), null);
  assert.equal(chordOf(key('k', { ctrl: true, shift: true })), 'Ctrl+Shift+K');
  assert.equal(chordOf({ key: '`', code: 'Backquote', ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }), 'Ctrl+`');
  assert.equal(chordOf({ key: 'Control', ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }), null);
});

test('the registry dispatches, honours user rebinding and clearing, and reports conflicts', () => {
  const r = new KeyRegistry();
  const ran: string[] = [];
  r.register({ id: 'a', title: 'Alpha', defaultKeys: 'Ctrl+K', run: () => ran.push('a') });
  r.register({ id: 'b', title: 'Beta', defaultKeys: 'Ctrl+K', run: () => ran.push('b') });
  assert.deepEqual(r.conflicts(), [{ chord: 'Ctrl+K', ids: ['a', 'b'] }]);
  r.dispatch(key('k', { ctrl: true }));
  assert.deepEqual(ran, ['a'], 'between two defaults the first registered wins');
  const refused = r.setUser('b', 'Ctrl+K');
  assert.equal(refused.ok, false, 'rebinding onto a chord in use is refused');
  const moved = r.setUser('b', 'Ctrl+J');
  assert.equal(moved.ok, true);
  assert.deepEqual(r.conflicts(), []);
  r.dispatch(key('j', { ctrl: true }));
  assert.deepEqual(ran, ['a', 'b']);
  r.setUser('a', null);
  assert.equal(r.dispatch(key('k', { ctrl: true })), false, 'cleared');
  assert.deepEqual(r.reset('a'), { b: 'Ctrl+J' });
  assert.equal(r.chordFor('a'), 'Ctrl+K');
});

test('reserved chords cannot be bound by the user; global bindings are never dispatched in-page', () => {
  const r = new KeyRegistry();
  let ran = 0;
  r.register({ id: 'x', title: 'X', defaultKeys: null, run: () => ran++ });
  for (const c of ['Alt+F4', 'Ctrl+Shift+I', 'Super+D', 'Ctrl+Super+K', 'F12']) {
    assert.equal(r.setUser('x', c).ok, false, c);
  }
  r.register({ id: 'g', title: 'Show Helm', defaultKeys: 'Ctrl+Alt+H', scope: 'global', run: () => ran++ });
  assert.equal(r.dispatch(key('h', { ctrl: true, alt: true })), false);
  assert.equal(r.setUser('x', 'Ctrl+Alt+H').ok, false, 'the global hotkey is in the same map');
  assert.equal(ran, 0);
});

test('bare keys never fire while typing; chorded ones do unless opted out', () => {
  const r = new KeyRegistry();
  const ran: string[] = [];
  r.register({ id: 'f', title: 'F', defaultKeys: 'F9', run: () => ran.push('f') });
  r.register({ id: 'c', title: 'C', defaultKeys: 'Ctrl+L', run: () => ran.push('c') });
  r.register({ id: 'n', title: 'N', defaultKeys: 'Ctrl+M', run: () => ran.push('n'), inInputs: false });
  const input = { tagName: 'INPUT' };
  assert.equal(r.dispatch({ key: 'F9', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, target: input as unknown as EventTarget }), false);
  r.dispatch(key('l', { ctrl: true }, input));
  r.dispatch(key('m', { ctrl: true }, input));
  assert.deepEqual(ran, ['c']);
});

test('the user map loads leniently', () => {
  const r = new KeyRegistry();
  r.register({ id: 'a', title: 'A', defaultKeys: 'Ctrl+K' });
  r.loadUserMap({ a: 'cmd+shift+k', junk: 5, bad: 'Hyper+X', cleared: null });
  assert.equal(r.chordFor('a'), 'Ctrl+Shift+K');
  assert.deepEqual(r.userMap(), { a: 'Ctrl+Shift+K', cleared: null });
  r.loadUserMap('nonsense');
  assert.equal(r.chordFor('a'), 'Ctrl+K');
});

test('toast slot: one at a time by priority; a hidden lower toast returns if still live', () => {
  let now = 0;
  const t = new ToastSlot(() => now);
  t.show({ kind: 'undo', text: 'Archived "A"' });
  const err = t.show({ kind: 'error', text: 'Boom' });
  assert.equal(t.current()?.kind, 'error');
  now = 1000;
  t.dismiss(err);
  assert.equal(t.current()?.kind, 'undo', 'still live, so it shows again');
  now = 3001;
  t.tick();
  assert.equal(t.current(), null, 'undo lives 3 s');
  t.show({ kind: 'info', text: 'a' });
  t.show({ kind: 'error', text: 'b' });
  assert.equal(t.current()?.text, 'b', 'error and info share the top slot; the newer replaces');
  now += 4999;
  t.tick();
  assert.equal(t.current()?.text, 'b');
  now += 2;
  t.tick();
  assert.equal(t.current(), null, 'errors live 5 s');
});

test('anchored placement flips above and clamps to the window', () => {
  const vp = { width: 800, height: 600 };
  assert.deepEqual(placeBelow({ left: 100, top: 100, width: 50, height: 20 }, { width: 344, height: 200 }, vp), { left: 100, top: 126, above: false });
  const flipped = placeBelow({ left: 700, top: 500, width: 50, height: 20 }, { width: 344, height: 200 }, vp);
  assert.equal(flipped.above, true);
  assert.equal(flipped.top, 294);
  assert.equal(flipped.left, 800 - 8 - 344);
  const m = placeAtPoint(790, 590, { width: 200, height: 100 }, vp);
  assert.deepEqual(m, { left: 590, top: 490, above: true });
});

test('arrow navigation skips disabled entries and wraps', () => {
  assert.equal(nextIndex(-1, 1, [false, true, false]), 0);
  assert.equal(nextIndex(0, 1, [false, true, false]), 2);
  assert.equal(nextIndex(2, 1, [false, true, false]), 0);
  assert.equal(nextIndex(0, -1, [false, true, false]), 2);
  assert.equal(nextIndex(0, 1, [true, true]), -1);
});

test('the breadcrumb takes the last folder segment on Windows and POSIX paths', () => {
  assert.equal(folderLabel('C:\\Users\\x\\proj'), 'proj');
  assert.equal(folderLabel('/home/x/proj/'), 'proj');
  assert.equal(folderLabel('C:/a/b'), 'b');
  assert.equal(folderLabel(null), '');
});

test('a window drag starts only on bare bar surface, left button, no Ctrl', () => {
  const bare = { closest: () => null };
  const onButton = { closest: (sel: string) => (sel.includes('button') ? {} : null) };
  assert.equal(startsDrag(bare, 0, false), true);
  assert.equal(startsDrag(onButton, 0, false), false);
  assert.equal(startsDrag(bare, 2, false), false, 'right button');
  assert.equal(startsDrag(bare, 0, true), false, 'Ctrl held');
  assert.equal(startsDrag(null, 0, false), false);
});
