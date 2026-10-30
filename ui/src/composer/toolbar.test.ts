import assert from 'node:assert/strict';
import { test } from 'node:test';
import { commitPatch, effortLabel, filterRows, formatContext, isLocalModel, localHint, localModelChoices, localModelName, localServerId, modelRows, shortName, stepEffort, thinkingLabel, triggerLabel } from './modelEffort.ts';
import { afterTurn, newPick, permissionTooltip, pickedMode, setPickedMode, shownMode } from './permission.ts';
import { sendLabel } from './keymap.ts';
import type { ModelCatalogue } from '../api/types.ts';

const cat: ModelCatalogue = {
  harness: 'claude',
  models: [
    { id: 'opus', name: 'Opus 5.5 (1M)', description: 'Most capable' },
    { id: 'haiku', name: 'Haiku 5', description: '' },
  ],
  efforts: ['low', 'medium', 'high', 'xhigh', 'max'],
  defaultEffort: 'xhigh',
  source: 'fallback',
  local: [],
};

test('the unified trigger reads "Model · Effort" with friendly effort names', () => {
  assert.equal(triggerLabel('Opus 5.5 (1M)', 'xhigh'), 'Opus 5.5 · Extra high');
  assert.equal(triggerLabel('Haiku 5', 'low'), 'Haiku 5 · Low');
  assert.equal(effortLabel('max'), 'Max');
  assert.equal(effortLabel('ultra'), 'ultra', 'an unknown level shows as itself');
  assert.equal(shortName('Sonnet 5'), 'Sonnet 5');
});

test('Left/Right step the effort and stop at the ends', () => {
  const e = cat.efforts;
  assert.equal(stepEffort(e, 'high', 1), 'xhigh');
  assert.equal(stepEffort(e, 'high', -1), 'medium');
  assert.equal(stepEffort(e, 'max', 1), 'max');
  assert.equal(stepEffort(e, 'low', -1), 'low');
  assert.equal(stepEffort(e, 'odd', 1), 'low', 'from an unknown level, right starts at the lowest');
  assert.equal(stepEffort(e, 'odd', -1), 'max');
});

test('rows: the catalogue, then the chat model as a disabled row when it is not listed', () => {
  const rows = modelRows(cat, 'opus');
  assert.deepEqual(
    rows.map((r) => [r.id, r.title, r.detail, r.disabled]),
    [
      ['opus', 'Opus 5.5', 'Claude Code · Most capable', false],
      ['haiku', 'Haiku 5', 'Claude Code', false],
    ],
  );
  const odd = modelRows(cat, 'claude-old');
  assert.equal(odd.length, 3);
  assert.deepEqual([odd[2]!.id, odd[2]!.disabled, odd[2]!.detail], ['claude-old', true, 'Not in the current model list']);
  assert.deepEqual(
    filterRows(rows, 'HAI').map((r) => r.id),
    ['haiku'],
  );
  assert.equal(filterRows(rows, '  ').length, 2);
});

const localCat: ModelCatalogue = {
  ...cat,
  local: [
    { id: 'local/strata/qwen-flash', name: 'Qwen Flash', server: 'Strata', kind: 'strata', contextTokens: 262144, loaded: true, reachable: true, startable: false },
    { id: 'local/strata/qwen-coder', name: 'Qwen Coder', server: 'Strata', kind: 'strata', contextTokens: 131072, reachable: true, startable: false },
    // Down, but Helm can start this Strata: the row stays choosable.
    { id: 'local/strata/down:14b', name: 'Qwen 14B', server: 'Strata', kind: 'strata', contextTokens: 131072, reachable: false, startable: true },
    { id: 'local/ollama/org/llama:3b', name: 'Llama 3B', server: 'Ollama', kind: 'ollama', contextTokens: 8192, reachable: true, startable: false },
    { id: 'local/office/big:30b', name: 'Big 30B', server: 'Ollama (office)', kind: 'ollama', reachable: false, startable: false },
  ],
};

test('local models: their own group with server and context, greyed when the server did not answer, fixed after the first turn', () => {
  // The id splits on the first two slashes only, the context reads like people write it, and effort reads as thinking.
  assert.deepEqual([isLocalModel('local/ollama/org/llama:3b'), isLocalModel('opus'), isLocalModel(null)], [true, false, false]);
  assert.deepEqual([localServerId('local/ollama/org/llama:3b'), localServerId('opus'), localServerId('local/x')], ['ollama', null, null]);
  assert.deepEqual([262144, 131072, 8192, 1_000_000, 512, undefined].map(formatContext), ['256k', '128k', '8k', '1M', '512', '']);
  assert.equal(triggerLabel('Qwen Flash', 'high', true), 'Qwen Flash · High thinking');
  assert.equal(triggerLabel('Qwen Flash', 'low', true), 'Qwen Flash · No thinking');
  assert.equal(thinkingLabel('xhigh'), 'Extra high thinking');

  // Before the first turn: every reachable row can be picked; the group headings only exist with local models.
  const open = modelRows(localCat, 'opus');
  assert.deepEqual(
    open.map((r) => [r.id, r.group, r.detail, r.disabled]),
    [
      ['opus', 'Claude', 'Claude Code · Most capable', false],
      ['haiku', 'Claude', 'Claude Code', false],
      ['local/strata/qwen-flash', 'Local', 'Strata · 256k · loaded', false],
      ['local/strata/qwen-coder', 'Local', 'Strata · 128k', false],
      ['local/strata/down:14b', 'Local', 'Strata · 128k · not running, starts when you use it', false],
      ['local/ollama/org/llama:3b', 'Local', 'Ollama · 8k', false],
      ['local/office/big:30b', 'Local', 'Ollama (office) · not answering', true],
    ],
  );
  assert.match(open[6]!.hint ?? '', /Ollama \(office\) did not answer/);
  assert.equal(open[4]!.hint, undefined, 'a Strata that can be started needs no warning tooltip');
  assert.equal(modelRows(cat, 'opus').some((r) => r.group), false);
  assert.deepEqual(filterRows(open, 'strata').map((r) => r.id), ['local/strata/qwen-flash', 'local/strata/qwen-coder', 'local/strata/down:14b']);

  // After the first turn: a Claude chat keeps Claude models, a local chat keeps its server.
  const enabled = (cur: string) => modelRows(localCat, cur, true).filter((r) => !r.disabled).map((r) => r.id);
  assert.deepEqual(enabled('opus'), ['opus', 'haiku']);
  assert.deepEqual(enabled('local/strata/qwen-flash'), ['local/strata/qwen-flash', 'local/strata/qwen-coder', 'local/strata/down:14b']);
  assert.deepEqual(enabled('local/ollama/org/llama:3b'), ['local/ollama/org/llama:3b']);
  assert.match(modelRows(localCat, 'opus', true)[2]!.hint ?? '', /fixed for this chat: it started on Claude/);
  assert.match(modelRows(localCat, 'local/strata/qwen-flash', true)[0]!.hint ?? '', /started on Strata/);
  // A chat on a model the lists no longer hold still shows it (disabled), under its own group.
  const gone = modelRows(localCat, 'local/strata/old', true);
  assert.deepEqual([gone.at(-1)!.id, gone.at(-1)!.group, gone.at(-1)!.disabled], ['local/strata/old', 'Local', true]);
  assert.equal(gone.at(-1)!.title, 'old', 'before the servers have been asked, a local model reads as its own name');
  assert.equal(localModelName('local/ollama/org/llama:3b'), 'org/llama:3b');
  // The servers are only asked on "Check servers": until then the picker says so, and says if the check found nothing.
  assert.deepEqual([localHint(cat, false), localHint(cat, true), localHint(localCat, false)], ['Local models: not checked yet', 'Local models: none found', 'Local models']);
  assert.equal(localHint(null, false), 'Local models: not checked yet');
  // A server that answered but has something to report says it in the footer instead.
  assert.equal(localHint(localCat, true, ['Strata answered but listed no models.']), 'Strata answered but listed no models.');
  assert.equal(localHint(localCat, false, ['', '  ', 'Strata could not be started.']), 'Strata could not be started.', 'the first real note wins');
  // Settings → Runtime's plain list gets the same models, with the server in the description.
  assert.deepEqual(localModelChoices(localCat.local).map((c) => [c.name, c.description]).slice(0, 2), [['Qwen Flash', 'Strata · 256k · loaded'], ['Qwen Coder', 'Strata · 128k']]);
  assert.deepEqual(localModelChoices(undefined), []);
});

test('a commit sends only what changed, and nothing when nothing did', () => {
  const now = { model: 'opus', effort: 'xhigh' };
  assert.equal(commitPatch(now, { model: 'opus', effort: 'xhigh' }), null);
  assert.deepEqual(commitPatch(now, { model: 'haiku', effort: 'xhigh' }), { model: 'haiku' });
  assert.deepEqual(commitPatch(now, { model: 'opus', effort: 'low' }), { effort: 'low' });
  assert.deepEqual(commitPatch(now, { model: 'haiku', effort: 'low' }), { model: 'haiku', effort: 'low' });
});

test('the permission chip shows a pick until the CLI reports it, then the live mode', () => {
  assert.deepEqual(shownMode(null, null), { mode: 'bypassPermissions', pending: false });
  assert.deepEqual(shownMode(null, 'default'), { mode: 'default', pending: false });
  assert.deepEqual(shownMode('plan', 'bypassPermissions'), { mode: 'plan', pending: true });
  assert.deepEqual(shownMode('plan', 'plan'), { mode: 'plan', pending: false });
  assert.match(permissionTooltip('plan', true), /Applies from the next turn/);
  assert.doesNotMatch(permissionTooltip('plan', false), /next turn/);
  setPickedMode('c1', newPick('acceptEdits', false));
  assert.equal(pickedMode('c1')?.mode, 'acceptEdits');
  assert.equal(pickedMode('c2'), null);
  setPickedMode('c1', null);
  assert.equal(pickedMode('c1'), null);
});

test('the icon Send button names what it will do', () => {
  assert.equal(sendLabel(false, 'asap'), 'Send');
  assert.equal(sendLabel(true, 'asap'), 'Steer');
  assert.equal(sendLabel(true, 'interrupt'), 'Interrupt');
});

test('a pick the CLI never reports is dropped at the turn end it should have applied by', () => {
  // Picked while idle: the next turn applies it, so its end is the deadline.
  const idle = newPick('default', false);
  assert.deepEqual(afterTurn(idle, 'default'), { pick: null, refused: false }, 'reported: nothing pending');
  assert.deepEqual(afterTurn(idle, 'bypassPermissions'), { pick: null, refused: true });
  // Picked mid-turn: the current turn's end is too early; the next one is the deadline.
  const mid = newPick('plan', true);
  const first = afterTurn(mid, 'bypassPermissions');
  assert.deepEqual(first, { pick: { mode: 'plan', turnsLeft: 1 }, refused: false });
  assert.deepEqual(afterTurn(first.pick, 'bypassPermissions'), { pick: null, refused: true });
  assert.deepEqual(afterTurn(null, 'plan'), { pick: null, refused: false });
});

test('references: twenty attachments in all, one chip per target, drops hit-tested in CSS pixels', async () => {
  const { addRef, dropLandsIn, parseFileDrop, roomLeft } = await import('./refs.ts');
  assert.equal(roomLeft(0, 0), 20);
  assert.equal(roomLeft(12, 8), 0);
  assert.equal(roomLeft(15, 9), 0, 'never negative');
  const a = { id: 'a', kind: 'file' as const, name: 'n.md', target: 'C:/n.md' };
  assert.equal(addRef([a], { ...a }).length, 1, 'the host answered with an id already shown');
  assert.equal(addRef([a], { ...a, id: 'b' }).length, 2);
  const box = { left: 100, top: 500, right: 920, bottom: 610 };
  assert.equal(dropLandsIn(box, 300, 1100, 2), true, 'physical 300,1100 at ratio 2 is CSS 150,550');
  assert.equal(dropLandsIn(box, 300, 1100, 1), false, 'the same point at ratio 1 is below the box');
  assert.equal(dropLandsIn(null, 300, 1100, 2), false);
  assert.deepEqual(parseFileDrop({ phase: 'drop', paths: ['C:/a', 3], x: 1, y: 2 }), { phase: 'drop', paths: ['C:/a'], x: 1, y: 2 });
  assert.equal(parseFileDrop({ phase: 'hover' }), null);
});
