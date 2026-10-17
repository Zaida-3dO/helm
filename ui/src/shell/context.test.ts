import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createApi } from '../api/client.ts';
import { EventBus } from '../api/events.ts';
import { chatOps } from '../api/ops.ts';
import { ChatStore } from '../state/store.ts';
import { parseSelection, ShellController } from './context.ts';
import { keys } from '../ui/keys.ts';
import { resolveTheme, parseAppearance } from './theme.ts';
import { filterPanes, registerSettingsPane, settingsPanes } from '../settings/registry.ts';

function harness(stored: Record<string, unknown> = {}) {
  const calls: { op: string; args: unknown }[] = [];
  const api = createApi(async (_cmd, payload) => {
    const op = String(payload.op);
    calls.push({ op, args: payload.args });
    if (op === 'preference_get') return stored[(payload.args as { key: string }).key] ?? null;
    return null;
  });
  const shell = new ShellController(api, new EventBus(), new ChatStore(), chatOps(api));
  return { shell, calls };
}

test('a stored selection is restored at launch; junk reads as nothing selected', async () => {
  const { shell } = harness({ selection: { chatId: 'c1', projectId: 'p1' } });
  await shell.restore();
  assert.deepEqual(shell.selection, { chatId: 'c1', projectId: 'p1' });
  assert.deepEqual(parseSelection({ chatId: 5 }), { chatId: null, projectId: null });
  assert.deepEqual(parseSelection('x'), { chatId: null, projectId: null });
});

test('a selection made before the restore answers is not overwritten', async () => {
  const { shell } = harness({ selection: { chatId: 'old', projectId: null } });
  shell.select('new');
  await shell.restore();
  assert.equal(shell.selection.chatId, 'new');
});

test('select persists the selection through the generic preference op', async () => {
  const { shell, calls } = harness();
  shell.select('c2', { projectId: 'p9' });
  await shell.flush();
  const w = calls.find((c) => c.op === 'preference');
  assert.deepEqual(w?.args, { key: 'selection', value: { chatId: 'c2', projectId: 'p9' } });
});

test('select notifies subscribers and carries a one-shot message focus', () => {
  const { shell } = harness();
  let n = 0;
  shell.subscribe(() => n++);
  shell.select('c3', { messageId: 'm1' });
  assert.equal(n, 1);
  assert.deepEqual(shell.takeFocus('c3'), { chatId: 'c3', messageId: 'm1' });
  assert.equal(shell.takeFocus('c3'), null, 'taken once');
  shell.select('c3');
  assert.equal(n, 1, 're-selecting the same chat with no focus is not a change');
});

test('registerCommand lands in the key registry and unregisters', () => {
  const { shell } = harness();
  let ran = 0;
  const off = shell.registerCommand('t.cmd', { title: 'T', keys: 'mod+shift+y', run: () => ran++ });
  assert.equal(keys.chordFor('t.cmd'), 'Ctrl+Shift+Y');
  assert.equal(keys.dispatch({ key: 'Y', code: 'KeyY', ctrlKey: true, metaKey: false, altKey: false, shiftKey: true }), true);
  assert.equal(ran, 1);
  off();
  assert.equal(keys.dispatch({ key: 'Y', code: 'KeyY', ctrlKey: true, metaKey: false, altKey: false, shiftKey: true }), false);
});

test('flush before restore does not overwrite the stored selection', async () => {
  const { shell, calls } = harness({ selection: { chatId: 'kept', projectId: null } });
  await shell.flush();
  assert.equal(calls.some((c) => c.op === 'preference'), false);
  await shell.restore();
  await shell.flush();
  assert.deepEqual(calls.find((c) => c.op === 'preference')?.args, { key: 'selection', value: { chatId: 'kept', projectId: null } });
});

test('dialogs: one open at a time; closing a different id does nothing', () => {
  const { shell } = harness();
  shell.openDialog('settings');
  shell.closeDialog('search');
  assert.equal(shell.openDialogId, 'settings');
  shell.openDialog('search');
  assert.equal(shell.openDialogId, 'search');
  shell.closeDialog();
  assert.equal(shell.openDialogId, null);
});

test('theme: system follows the OS, explicit modes win, junk is system', () => {
  assert.equal(resolveTheme('system', true), 'dark');
  assert.equal(resolveTheme('system', false), 'light');
  assert.equal(resolveTheme('light', true), 'light');
  assert.equal(resolveTheme(parseAppearance('purple'), true), 'dark');
});

test('settings panes sort by order then title, replace by id, and filter by title', () => {
  const offB = registerSettingsPane({ id: 'b', title: 'Beta', order: 2, render: () => null });
  const offA = registerSettingsPane({ id: 'a', title: 'Alpha', order: 2, render: () => null });
  const offS = registerSettingsPane({ id: 's', title: 'Shell', order: 1, render: () => null });
  assert.deepEqual(settingsPanes().map((p) => p.id), ['s', 'a', 'b']);
  registerSettingsPane({ id: 'b', title: 'Beta', order: 0, render: () => null });
  assert.deepEqual(settingsPanes().map((p) => p.id), ['b', 's', 'a']);
  offB(); // a stale unregister does not remove the replacement
  assert.equal(settingsPanes().length, 3);
  assert.deepEqual(filterPanes(settingsPanes(), ' SHE').map((p) => p.id), ['s']);
  offA();
  offS();
});
