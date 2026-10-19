import assert from 'node:assert/strict';
import { test } from 'node:test';
import { commitPatch, effortLabel, filterRows, modelRows, shortName, stepEffort, triggerLabel } from './modelEffort.ts';
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
  assert.equal(sendLabel(true, 'queue'), 'Queue');
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
