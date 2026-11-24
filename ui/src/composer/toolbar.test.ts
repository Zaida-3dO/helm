import assert from 'node:assert/strict';
import { test } from 'node:test';
import { commitPatch, effortLabel, filterRows, formatContext, isLocalModel, localHint, localModelChoices, localModelName, localServerId, modelRows, shortName, stepEffort, thinkingLabel, triggerLabel, triggerParts, withDefaultRow } from './modelEffort.ts';
import { afterTurn, newPick, permissionTooltip, pickedMode, setPickedMode, shownMode } from './permission.ts';
import { sendLabel } from './keymap.ts';
import { prettyLocalModelId, prettyModelName } from '../state/localModel.ts';
import { ensureLabels, invalidateLocalLabels, modelDisplayName, primeLocalLabels } from '../state/modelNames.ts';
import type { Api } from '../api/client.ts';
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
    { id: 'local/strata/qwen-flash', name: 'Qwen Flash', model: 'qwen-flash', server: 'Strata', kind: 'strata', contextTokens: 262144, loaded: true, reachable: true, startable: false },
    { id: 'local/strata/qwen-coder', name: 'Qwen Coder', model: 'qwen-coder', server: 'Strata', kind: 'strata', contextTokens: 131072, reachable: true, startable: false },
    // Down, but Helm can start this Strata: the row stays choosable.
    { id: 'local/strata/down:14b', name: 'Qwen 14B', model: 'down:14b', server: 'Strata', kind: 'strata', contextTokens: 131072, reachable: false, startable: true },
    { id: 'local/ollama/org/llama:3b', name: 'Llama 3B', model: 'org/llama:3b', server: 'Ollama', kind: 'ollama', contextTokens: 8192, reachable: true, startable: false },
    { id: 'local/office/big:30b', name: 'Big 30B', model: 'big:30b', server: 'Ollama (office)', kind: 'ollama', reachable: false, startable: false },
  ],
};

test('local models: their own group with server and context, greyed only when the server did not answer', () => {
  // The id splits on the first two slashes only, the context reads like people write it, and effort reads as thinking.
  assert.deepEqual([isLocalModel('local/ollama/org/llama:3b'), isLocalModel('opus'), isLocalModel(null)], [true, false, false]);
  assert.deepEqual([localServerId('local/ollama/org/llama:3b'), localServerId('opus'), localServerId('local/x')], ['ollama', null, null]);
  assert.deepEqual([262144, 131072, 8192, 1_000_000, 512, undefined].map(formatContext), ['256k', '128k', '8k', '1M', '512', '']);
  assert.equal(triggerLabel('Qwen Flash', 'xhigh', true), 'Qwen Flash · Extra high');
  assert.equal(triggerLabel('Qwen 3.8 (fast)', 'high', true), 'Qwen 3.8 (fast) · High', 'a local alias is kept whole');
  assert.equal(triggerLabel('Qwen Flash', 'low', true), 'Qwen Flash · No thinking');
  // The name and the effort word are separate, so a narrow toolbar can drop the effort and keep the name.
  assert.deepEqual(triggerParts('Opus 5.5 (1M)', 'xhigh'), { name: 'Opus 5.5', rest: 'Extra high' });
  assert.deepEqual(triggerParts('Qwen Flash', 'low', true), { name: 'Qwen Flash', rest: 'No thinking' });
  assert.equal(thinkingLabel('xhigh'), 'Extra high thinking');

  // Before the first turn: every reachable row can be picked; the group headings only exist with local models.
  const open = modelRows(localCat, 'opus');
  assert.deepEqual(
    open.map((r) => [r.id, r.group, r.detail, r.disabled]),
    [
      ['opus', 'Claude', 'Claude Code · Most capable', false],
      ['haiku', 'Claude', 'Claude Code', false],
      ['local/strata/qwen-flash', 'Local', 'qwen-flash · Strata · 256k · loaded', false],
      ['local/strata/qwen-coder', 'Local', 'qwen-coder · Strata · 128k', false],
      ['local/strata/down:14b', 'Local', 'down:14b · Strata · 128k · not running, starts when you use it', false],
      ['local/ollama/org/llama:3b', 'Local', 'org/llama:3b · Ollama · 8k', false],
      ['local/office/big:30b', 'Local', 'big:30b · Ollama (office) · not answering', true],
    ],
  );
  assert.match(open[6]!.hint ?? '', /Ollama \(office\) did not answer/);
  assert.equal(open[4]!.hint, undefined, 'a Strata that can be started needs no warning tooltip');
  assert.equal(modelRows(cat, 'opus').some((r) => r.group), false);
  assert.deepEqual(filterRows(open, 'strata').map((r) => r.id), ['local/strata/qwen-flash', 'local/strata/qwen-coder', 'local/strata/down:14b']);

  // A chat at any point can move its model anywhere: crossing between Claude and a local server,
  // or between servers, greys nothing out — the next turn just runs on the new model, carrying the
  // conversation. The only greyed rows are servers that did not answer.
  const disabledIds = (cur: string) => modelRows(localCat, cur).filter((r) => r.disabled).map((r) => r.id);
  assert.deepEqual(disabledIds('opus'), ['local/office/big:30b']);
  assert.deepEqual(disabledIds('local/strata/qwen-flash'), ['local/office/big:30b'], 'a local chat keeps every Claude row pickable');
  assert.deepEqual(disabledIds('local/ollama/org/llama:3b'), ['local/office/big:30b'], 'rows on other servers stay pickable');
  // A chat on a model the lists no longer hold still shows it (disabled), under its own group.
  const gone = modelRows(localCat, 'local/strata/old');
  assert.deepEqual([gone.at(-1)!.id, gone.at(-1)!.group, gone.at(-1)!.disabled], ['local/strata/old', 'Local', true]);
  assert.equal(gone.at(-1)!.title, 'Old', 'before the servers have been asked, a local model reads as the short name made from it, never the raw id');
  assert.equal(localModelName('local/ollama/org/llama:3b'), 'org/llama:3b');
  // The servers are only asked on "Check servers": until then the picker says so, and says if the check found nothing.
  assert.deepEqual([localHint(cat, false), localHint(cat, true), localHint(localCat, false)], ['Local models: not checked yet', 'Local models: none found', 'Local models']);
  assert.equal(localHint(null, false), 'Local models: not checked yet');
  // A server that answered but has something to report says it in the footer instead.
  assert.equal(localHint(localCat, true, ['Strata answered but listed no models.']), 'Strata answered but listed no models.');
  assert.equal(localHint(localCat, false, ['', '  ', 'Strata could not be started.']), 'Strata could not be started.', 'the first real note wins');
  // Settings → Runtime's plain list gets the same models, with the server in the description.
  assert.deepEqual(localModelChoices(localCat.local).map((c) => [c.name, c.description]).slice(0, 2), [['Qwen Flash', 'qwen-flash · Strata · 256k · loaded'], ['Qwen Coder', 'qwen-coder · Strata · 128k']]);
  assert.deepEqual(localModelChoices(undefined), []);
  // The new-chat default can be "Claude Code's default": a first row, under Claude when the list has headings.
  const withDefault = withDefaultRow(modelRows(localCat, null), 'default');
  assert.deepEqual([withDefault[0]!.id, withDefault[0]!.title, withDefault[0]!.group, withDefault.length], ['default', "Claude Code's default", 'Claude', modelRows(localCat, null).length + 1]);
  assert.equal(withDefaultRow(modelRows(cat, null), 'default')[0]!.group, undefined, 'no headings when the list has none');
  // Reply length steps along its three stops and stops at the ends, like the effort.
  const levels = ['low', 'medium', 'high'];
  assert.deepEqual([stepEffort(levels, 'low', -1), stepEffort(levels, 'low', 1), stepEffort(levels, 'high', 1), stepEffort(levels, 'unknown', 1)], ['low', 'medium', 'high', 'low']);
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

test('a local model id reads as a person would say it, not as the server writes it', () => {
  for (const [raw, want] of [
    ['qwen3.8-flash-next-iq3_s', 'Qwen 3.8'],
    ['qwen3:32b', 'Qwen 3 32B'],
    ['qwen3:8b-instruct-q4_K_M', 'Qwen 3 8B'],
    ['llama3.2:latest', 'Llama 3.2'],
    ['hf.co/org/llama:3b', 'Llama 3B'],
    ['llama-3.1-8b-instruct-q8_0.gguf', 'Llama 3.1 8B'],
    ['gpt-oss-20b', 'Gpt 20B'],
    ['gemma-3-27b-it-q4-k-m', 'Gemma 3 27B'],
    ['mistral', 'Mistral'],
    ['12345', '12345'],
    ['', ''],
  ] as const) {
    assert.equal(prettyModelName(raw), want, raw);
  }
  // A whole model id: the server's segment is skipped, the model's own name prettified (it may hold / and :).
  assert.equal(prettyLocalModelId('local/strata/qwen3.8-flash-next-iq3_s'), 'Qwen 3.8');
  assert.equal(prettyLocalModelId('local/ollama/hf.co/org/llama:3b'), 'Llama 3B');
});

test('an alias rides with the default name in the picker and stands alone on the trigger', () => {
  const aliased: ModelCatalogue = {
    ...cat,
    local: [
      { id: 'local/strata/qwen3.8-flash-next-iq3_s', name: 'Jane', model: 'qwen3.8-flash-next-iq3_s', server: 'Strata', kind: 'strata', contextTokens: 262144, reachable: true, startable: false },
      { id: 'local/strata/qwen3.8-coder-iq3_s', name: 'Qwen 3.8 Coder', model: 'qwen3.8-coder-iq3_s', server: 'Strata', kind: 'strata', reachable: true, startable: false },
    ],
  };
  const rows = modelRows(aliased, null);
  const [jane, coder] = rows.slice(2);
  // The row says whose pet name this is; the trigger and chip wear the alias alone.
  assert.deepEqual([jane!.title, jane!.name], ['Jane · Qwen 3.8', 'Jane']);
  // A default name lengthened by the model's own distinguishing words (a clash) is not a user name.
  assert.deepEqual([coder!.title, coder!.name], ['Qwen 3.8 Coder', 'Qwen 3.8 Coder']);
  assert.equal(triggerParts(jane!.name, 'high', true).name, 'Jane');
  // The raw model name keeps its place in the row's detail.
  assert.match(jane!.detail, /^qwen3\.8-flash-next-iq3_s · Strata · 256k/);
});

test('model display names: the settings alias first, else the name made from the id, on either PC', async () => {
  const state = {
    modelNames: [
      { id: 'local/strata/qwen3.8-flash-next-iq3_s', model: 'qwen3.8-flash-next-iq3_s', server: 'Strata', defaultName: 'Qwen 3.8', alias: 'Jane' },
      { id: 'local/ollama/llama3.2:latest', model: 'llama3.2:latest', server: 'Ollama', defaultName: 'Llama 3.2' },
    ],
  } as unknown as Parameters<typeof primeLocalLabels>[0];
  primeLocalLabels(state);
  assert.equal(modelDisplayName('local/strata/qwen3.8-flash-next-iq3_s'), 'Jane');
  assert.equal(modelDisplayName('local/ollama/llama3.2:latest'), 'Llama 3.2');
  // A local model nobody listed yet still reads as its short name, never as its id.
  assert.equal(modelDisplayName('local/strata/gemma-3-27b-it-q4_k_m'), 'Gemma 3 27B');
  // A Claude id has no name of its own here; the catalogue carries those.
  assert.equal(modelDisplayName('opus'), null);
  // Another PC's labels come from that PC's own list, so its aliases show here as they do there.
  const api = (async (op: string) => {
    if (op === 'model_catalogue') return { local: [{ id: 'local/strata/qwen3.8-coder-iq3_s', name: 'Rex' }] };
    throw new Error(`unexpected op ${op}`);
  }) as unknown as Api;
  await ensureLabels(api, 'pc-2');
  assert.equal(modelDisplayName('local/strata/qwen3.8-coder-iq3_s', 'pc-2'), 'Rex');
  assert.equal(modelDisplayName('local/strata/qwen3.8-coder-iq3_s'), 'Qwen 3.8', 'this PC never heard of that model, so its own short name answers');
  invalidateLocalLabels();
  assert.equal(modelDisplayName('local/ollama/llama3.2:latest'), 'Llama 3.2', 'an invalidation still leaves the short-name fallback');
  assert.equal(modelDisplayName('local/strata/qwen3.8-flash-next-iq3_s'), 'Qwen 3.8', 'and with it the alias is gone until the settings are read again');
});
