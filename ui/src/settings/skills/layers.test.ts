import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { SkillSource } from './api.ts';
import {
  currentLayer,
  defaultLayer,
  layerLabel,
  layerNote,
  layerOptions,
  moveTargets,
  projectsLabel,
  rulesBudgetNote,
  skillsFromSource,
  sourceLabel,
  toggleProject,
} from './layers.ts';

const profiles = [
  { id: 'p-sam', name: 'Sam', isHome: false },
  { id: 'p-alex', name: 'Alex', isHome: true },
];
const options = layerOptions(profiles);

test('layers: Everyone first, then the home profile, then the others', () => {
  assert.deepEqual(
    options.map((o) => [o.key, o.label, o.testSuffix]),
    [
      ['everyone', 'Everyone', 'everyone'],
      ['profile:p-alex', 'Alex', 'p-alex'],
      ['profile:p-sam', 'Sam', 'p-sam'],
    ],
  );
  assert.deepEqual(layerOptions([]).map((o) => o.key), ['everyone']);
});

test('layers: the opening layer, the shown layer and the move targets', () => {
  // [active, home, expected]
  for (const [active, home, want] of [
    ['p-sam', 'p-alex', 'profile:p-sam'],
    [null, 'p-alex', 'profile:p-alex'],
    ['gone', 'p-alex', 'profile:p-alex'],
    [null, null, 'everyone'],
  ] as const) {
    assert.equal(defaultLayer(options, active, home), want, `${active}/${home}`);
  }
  assert.equal(currentLayer(options, 'profile:p-sam', 'everyone'), 'profile:p-sam');
  assert.equal(currentLayer(options, 'profile:deleted', 'everyone'), 'everyone');
  assert.equal(currentLayer(options, null, 'profile:p-alex'), 'profile:p-alex');
  assert.deepEqual(moveTargets(options, 'profile:p-alex').map((o) => o.key), ['everyone', 'profile:p-sam']);
  assert.equal(layerLabel(options, 'profile:p-sam'), 'Sam');
  // [layer, own profile's layer, text starts with, another person's list]
  for (const [layer, own, starts, other] of [
    ['everyone', 'profile:p-alex', 'Loaded into every chat', false],
    ['profile:p-alex', 'profile:p-alex', "Loaded into Alex's chats only", false],
    ['profile:p-sam', 'profile:p-alex', "This is Sam's list, not yours", true],
    ['profile:p-sam', null, "Loaded into Sam's chats only", false],
  ] as const) {
    const note = layerNote(options, layer, own);
    assert.ok(note.text.startsWith(starts), `${layer}/${own}: ${note.text}`);
    assert.equal(note.other, other, `${layer}/${own}`);
  }
  assert.equal(layerLabel(options, 'profile:deleted'), 'a removed profile');
});

test('layers: where a skill came from, which projects a folder reaches, how many skills it gave', () => {
  const sources: SkillSource[] = [{ id: 's1', path: 'C:\\team\\shared-skills', layer: 'everyone', projects: 'all', enabled: true }];
  assert.equal(sourceLabel('folder', sources), 'In Helm');
  assert.equal(sourceLabel('linked:s1', sources), 'Linked: shared-skills');
  assert.equal(sourceLabel('linked:gone', sources), 'Linked folder');

  const names = new Map([['pr-1', 'Shop']]);
  assert.equal(projectsLabel('all', names), 'All projects');
  assert.equal(projectsLabel([], names), 'No projects');
  assert.equal(projectsLabel(['pr-1', 'pr-2'], names), 'Shop, pr-2');

  assert.deepEqual(toggleProject(['a'], 'b'), ['a', 'b']);
  assert.deepEqual(toggleProject(['a', 'b'], 'a'), ['b']);
  assert.equal(skillsFromSource([{ source: 'folder' }, { source: 'linked:s1' }, { source: 'linked:s1' }], 's1'), 2);
});

test('layers: the rules budget note appears at 80% of the cap and names both figures', () => {
  assert.equal(rulesBudgetNote(undefined), null, 'an older host says nothing');
  assert.equal(rulesBudgetNote({ usedBytes: 4096, capBytes: 0 }), null, 'a cap of nothing warns about nothing');
  // 80% of 32 KiB is 26,214.4 bytes: just under stays quiet, just over reads in KB.
  assert.equal(rulesBudgetNote({ usedBytes: 26_213, capBytes: 32_768 }), null);
  assert.equal(
    rulesBudgetNote({ usedBytes: 26_215, capBytes: 32_768 }),
    'Rules kept whole use 26 KB of the 32 KB chats get; past that the rest are only listed by name.',
  );
  assert.equal(
    rulesBudgetNote({ usedBytes: 27_648, capBytes: 32_768 }),
    'Rules kept whole use 27 KB of the 32 KB chats get; past that the rest are only listed by name.',
  );
  assert.equal(
    rulesBudgetNote({ usedBytes: 40_000, capBytes: 32_768 }),
    'Rules kept whole use 39 KB of the 32 KB chats get; past that the rest are only listed by name.',
    'past the cap it still says the whole-rule figure',
  );
});
