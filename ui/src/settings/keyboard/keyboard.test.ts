import { test } from 'node:test';
import assert from 'node:assert/strict';
import { conflictedIds, groupKeyboardRows, keyboardRows, sectionOf } from './model.ts';
import { hintLabel, isAssignable, KeyRegistry } from '../../ui/keys.ts';

const row = (id: string, title: string, chord: string | null, scope?: 'app' | 'global') => ({ id, title, chord, custom: false, scope });

test('the pane lists app commands by title, filtered by title or chord; the global hotkey is not listed', () => {
  const list = [row('b', 'Toggle terminal', 'Ctrl+`'), row('a', 'Fork chat', 'Ctrl+Shift+F'), row('g', 'Show or hide Helm', 'Ctrl+Alt+H', 'global'), row('n', 'Next chat', 'Ctrl+Tab')];
  assert.deepEqual(
    keyboardRows(list, '').map((r) => r.id),
    ['a', 'n', 'b'],
  );
  assert.deepEqual(
    keyboardRows(list, 'shift+f').map((r) => r.id),
    ['a'],
  );
  assert.deepEqual(
    keyboardRows(list, 'TERMINAL').map((r) => r.id),
    ['b'],
  );
});

test('conflicting ids are collected from every clash', () => {
  assert.deepEqual([...conflictedIds([{ ids: ['a', 'b'] }, { ids: ['c', 'd'] }])].sort(), ['a', 'b', 'c', 'd']);
});

test('a hint shows the live chord, or just the label when unbound', () => {
  assert.equal(hintLabel('Fork chat', 'Ctrl+Shift+F'), 'Fork chat · Ctrl+Shift+F');
  assert.equal(hintLabel('Fork chat', null), 'Fork chat');
});

test('a user shortcut needs Ctrl or Alt, or a function key', () => {
  assert.equal(isAssignable('Ctrl+K'), true);
  assert.equal(isAssignable('Alt+Shift+R'), true);
  assert.equal(isAssignable('F5'), true);
  assert.equal(isAssignable('Shift+F24'), true);
  assert.equal(isAssignable('Shift+A'), false);
  assert.equal(isAssignable('A'), false);
  assert.equal(isAssignable('F25'), false);
  assert.equal(isAssignable(null), false);
});

test('a rebinding moves the hint the menus and tooltips read', () => {
  const k = new KeyRegistry();
  k.register({ id: 'chat.fork', title: 'Fork chat', defaultKeys: 'Ctrl+Shift+F' });
  assert.equal(hintLabel('Fork chat', k.chordFor('chat.fork')), 'Fork chat · Ctrl+Shift+F');
  const r = k.setUser('chat.fork', 'Ctrl+Alt+K');
  assert.equal(r.ok, true);
  assert.equal(hintLabel('Fork chat', k.chordFor('chat.fork')), 'Fork chat · Ctrl+Alt+K');
  k.setUser('chat.fork', null);
  assert.equal(hintLabel('Fork chat', k.chordFor('chat.fork')), 'Fork chat');
});

test('Ctrl+Tab and Ctrl+Shift+Tab are chords the registry can dispatch', () => {
  const k = new KeyRegistry();
  const got: string[] = [];
  k.register({ id: 'chat.next', title: 'Next chat', defaultKeys: 'Ctrl+Tab', run: () => got.push('next') });
  k.register({ id: 'chat.previous', title: 'Previous chat', defaultKeys: 'Ctrl+Shift+Tab', run: () => got.push('prev') });
  const ev = (shiftKey: boolean) => ({ key: 'Tab', code: 'Tab', ctrlKey: true, shiftKey, altKey: false, metaKey: false });
  assert.equal(k.dispatch(ev(false)), true);
  assert.equal(k.dispatch(ev(true)), true);
  assert.deepEqual(got, ['next', 'prev']);
});

test('commands are grouped into sections by id, and empty sections are left out', () => {
  const ids: [string, string][] = [
    ['chat.new', 'Workspace'],
    ['sidebar.toggle', 'Workspace'],
    ['terminal.toggle', 'Workspace'],
    ['search.toggle', 'Workspace'],
    ['chat.find', 'Conversation'],
    ['chat.archive', 'Conversation'],
    ['composer.modelPicker', 'Composer'],
    ['settings.toggle', 'Window'],
    ['something.new', 'Window'],
  ];
  for (const [id, want] of ids) assert.equal(sectionOf(id), want, id);
  assert.deepEqual(
    groupKeyboardRows([{ id: 'settings.toggle' }, { id: 'chat.new' }, { id: 'chat.find' }]).map((g) => [g.section, g.rows.length]),
    [
      ['Workspace', 1],
      ['Conversation', 1],
      ['Window', 1],
    ],
  );
}
);
