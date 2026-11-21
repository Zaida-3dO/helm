import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SETTINGS_TAB_GROUPS, OTHER_GROUP_TITLE, groupSettingsTabs, settingsTabStrip, scrollEdges } from './layout.ts';

// Every settings pane registers with its own order, so the tab order never depends on which file
// happens to be imported first. This reads the registrations from the source.
const SRC = fileURLToPath(new URL('..', import.meta.url));

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) return files(p);
    return /\.(ts|tsx)$/.test(n) && !n.endsWith('.test.ts') ? [p] : [];
  });
}

function registrations(): { id: string; order: number }[] {
  const found: { id: string; order: number }[] = [];
  for (const f of files(SRC)) {
    for (const m of readFileSync(f, 'utf8').matchAll(/registerSettingsPane\(\{\s*id:\s*'([^']+)'[^}]*?order:\s*(\d+)/g)) {
      found.push({ id: m[1]!, order: Number(m[2]) });
    }
  }
  return found;
}

test('every settings pane has a unique order', () => {
  const seen = new Map<number, string>();
  for (const r of registrations()) {
    assert.equal(seen.get(r.order), undefined, `pane "${r.id}" shares order ${r.order} with "${seen.get(r.order)}"`);
    seen.set(r.order, r.id);
  }
  assert.ok(seen.size >= 10, `found only ${seen.size} pane registrations; the pattern no longer matches the source`);
});

test('every settings pane sits in exactly one tab group, and unknown ids land in Other', () => {
  const ids = registrations().map((r) => r.id);
  assert.ok(ids.length >= 10, `found only ${ids.length} pane registrations`);
  const listed = new Set<string>();
  for (const g of SETTINGS_TAB_GROUPS) {
    for (const id of g.ids) {
      assert.ok(!listed.has(id), `pane id "${id}" is listed in two tab groups`);
      listed.add(id);
    }
  }
  const groups = groupSettingsTabs(ids.map((id) => ({ id })));
  const flat = groups.flatMap((g) => g.panes.map((p) => p.id));
  assert.equal(new Set(flat).size, flat.length, 'a pane shows in more than one group');
  assert.deepEqual([...flat].sort(), [...ids].sort(), 'the groups together show every registered pane once');
  const unknown = groupSettingsTabs([{ id: 'a-unknown-pane' }, { id: 'chat' }]);
  assert.deepEqual(unknown.at(-1), { title: OTHER_GROUP_TITLE, panes: [{ id: 'a-unknown-pane' }] });
});

test('no registered pane needs an Other group; the Gatekeeper pane sits under Chat & agents', () => {
  const chatGroup = SETTINGS_TAB_GROUPS.find((g) => g.title === 'Chat & agents');
  assert.ok(chatGroup?.ids.includes('gatekeeper'), 'the Gatekeeper pane is not in Chat & agents');
  const ids = registrations().map((r) => r.id);
  const groups = groupSettingsTabs(ids.map((id) => ({ id })));
  assert.ok(!groups.some((g) => g.title === OTHER_GROUP_TITLE), `a registered pane fell into Other: ${groups.at(-1)?.panes.map((p) => p.id).join(', ')}`);
});

test('the two-row tab bar shows the chosen group, then the panes of that group', () => {
  const panes = [{ id: 'general' }, { id: 'appearance' }, { id: 'chat' }, { id: 'gatekeeper' }, { id: 'a-brand-new-pane' }];
  // [clicked group, clicked pane, the panes row two shows]
  const rows: [string | null, string | null, string[]][] = [
    // Nothing clicked yet: the first pane's group.
    [null, null, ['general', 'appearance']],
    // A group click shows that group's panes.
    ['Chat & agents', 'chat', ['chat', 'gatekeeper']],
    ['Machine & updates', 'general', ['general', 'appearance']], // the pane decides while it is in another group
    // A clicked pane that no longer exists falls back to the first pane's group.
    ['Chat & agents', 'a-gone-pane', ['general', 'appearance']],
    // An unknown pane stays reachable through Other.
    ['Chat & agents', 'a-brand-new-pane', ['a-brand-new-pane']],
  ];
  for (const [group, pane, want] of rows) {
    const strip = settingsTabStrip(panes, group, pane);
    assert.deepEqual(strip.activeGroup?.panes.map((p) => p.id), want, `${group}/${pane}`);
  }
  // The body shows the clicked pane; a clicked pane in another group decides its own row, it is
  // never silently switched.
  assert.equal(settingsTabStrip(panes, 'Chat & agents', 'gatekeeper').current?.id, 'gatekeeper');
  assert.equal(settingsTabStrip(panes, 'Chat & agents', 'general').current?.id, 'general');
  // A group click (which carries its first pane) shows that pane.
  assert.equal(settingsTabStrip(panes, 'Chat & agents', 'chat').current?.id, 'chat');
});

test('scroll fades show only on the edges that have more content', () => {
  const rows: [number, number, number, { top: boolean; bottom: boolean }][] = [
    [0, 400, 400, { top: false, bottom: false }], // fits
    [0, 400, 900, { top: false, bottom: true }], // at the top
    [200, 400, 900, { top: true, bottom: true }], // in the middle
    [500, 400, 900, { top: true, bottom: false }], // at the end
    [499.4, 400, 900, { top: true, bottom: false }], // fractional pixels at the end
  ];
  for (const [top, client, scroll, want] of rows) {
    assert.deepEqual(scrollEdges(top, client, scroll), want, `${top}/${client}/${scroll}`);
  }
});
