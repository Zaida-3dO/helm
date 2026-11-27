import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { RuleRow, SkillSource } from './api.ts';
import type { RuleWhen } from '../../api/generated/skills/RuleWhen.ts';
import {
  currentLayer,
  dateLabel,
  defaultLayer,
  filterRules,
  layerLabel,
  layerNote,
  layerOptions,
  moveTargets,
  projectsLabel,
  ruleDates,
  ruleFlags,
  ruleHasTrigger,
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

const noWhen = (): RuleWhen => ({
  show: null, otherwise: null, before: null,
  folders: [], hosts: [], pcs: [], tools: [], commands: [], kinds: [], people: [],
  from: null, until: null, review: null, problems: [],
});
const rule = (path: string, when?: RuleWhen, whenSummary?: string): RuleRow => ({
  path, title: path, size: 10, revision: '1', tooLarge: false, always: false, ...(when ? { when } : {}), ...(whenSummary ? { whenSummary } : {}),
});

test('rules: trigger dates, labels and badges read a header the way the host does', () => {
  const today = '2026-11-27';
  // [when, expired, notYet, reviewDue]
  for (const [w, expired, notYet, reviewDue] of [
    [noWhen(), false, false, false],
    [{ ...noWhen(), until: '2026-11-27' }, false, false, false], // the last day still counts
    [{ ...noWhen(), until: '2026-11-26' }, true, false, false],
    [{ ...noWhen(), from: '2026-11-28' }, false, true, false],
    [{ ...noWhen(), from: '2026-11-27' }, false, false, false],
    [{ ...noWhen(), review: '2026-11-26' }, false, false, true],
    [{ ...noWhen(), review: '2026-11-27' }, false, false, false], // the ask comes after the day
  ] as const) {
    const d = ruleDates(w, today);
    assert.equal(d.expired, expired, w.until ?? w.from ?? w.review ?? '');
    assert.equal(d.notYet, notYet, w.from ?? '');
    assert.equal(d.reviewDue, reviewDue, w.review ?? '');
  }
  // [iso, label] — the year shows only when it is not today's.
  for (const [iso, want] of [
    ['2026-12-31', '31 Dec'],
    ['2027-01-05', '5 Jan 2027'],
    ['2026-02-09', '9 Feb'],
    ['', ''],
    ['banana', 'banana'],
    ['2026-13-01', '2026-13-01'],
  ] as const) {
    assert.equal(dateLabel(iso, today), want, iso);
  }
  assert.deepEqual(ruleFlags(rule('rules/a.md', { ...noWhen(), until: '2026-11-01' }), today).map((b) => b.kind), ['expired']);
  // A review flag never rides with an Expired one, and problems ride beside either.
  assert.deepEqual(
    ruleFlags(rule('rules/b.md', { ...noWhen(), until: '2026-11-01', review: '2026-10-01', problems: ['tools: bad value'] }), today).map((b) => b.kind),
    ['expired', 'problem'],
  );
  assert.deepEqual(ruleFlags(rule('rules/c.md', { ...noWhen(), from: '2026-12-01' }), today).map((b) => b.kind), ['upcoming']);
});

test('rules: the filter chips, the search and the sort of due rows', () => {
  const today = '2026-11-27';
  const plain = rule('rules/plain.md');
  const legacy = rule('rules/legacy.md', undefined, 'old host: no when field at all');
  const trigger = rule('rules/toll.md', { ...noWhen(), commands: ['taskkill'] }, 'Bash: taskkill, pkill');
  const expired = rule('rules/gone.md', { ...noWhen(), until: '2026-10-31' });
  const review = rule('rules/stale.md', { ...noWhen(), review: '2026-10-31' });
  const rules = [plain, trigger, expired, legacy, review];

  assert.equal(ruleHasTrigger(undefined), false, 'a row without the field reads as no trigger');
  assert.equal(ruleHasTrigger(noWhen()), false);
  assert.equal(ruleHasTrigger({ ...noWhen(), show: 'line' }), true);
  assert.equal(ruleHasTrigger({ ...noWhen(), hosts: ['nas1'] }), true);

  const paths = (filter: string, query = '') => filterRules(rules, filter as never, query, today).map((r) => r.path);
  // Due rows come first; the rest keep their order.
  assert.deepEqual(paths('all'), ['rules/gone.md', 'rules/stale.md', 'rules/plain.md', 'rules/toll.md', 'rules/legacy.md']);
  assert.deepEqual(paths('plain'), ['rules/plain.md', 'rules/legacy.md']);
  assert.deepEqual(paths('triggered'), ['rules/stale.md', 'rules/toll.md']);
  assert.deepEqual(paths('expired'), ['rules/gone.md']);
  assert.deepEqual(paths('review'), ['rules/stale.md']);
  // The search reads the title, the file name and the trigger summary.
  assert.deepEqual(paths('all', 'toll'), ['rules/toll.md']);
  assert.deepEqual(paths('all', 'taskkill'), ['rules/toll.md']);
  assert.deepEqual(paths('plain', 'taskkill'), []);
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
