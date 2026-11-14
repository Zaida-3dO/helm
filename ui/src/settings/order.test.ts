import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SETTINGS_TAB_GROUPS, OTHER_GROUP_TITLE, groupSettingsTabs, scrollEdges } from './layout.ts';

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
