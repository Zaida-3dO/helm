import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

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

test('every settings pane has a unique order', () => {
  const seen = new Map<number, string>();
  let count = 0;
  for (const f of files(SRC)) {
    for (const m of readFileSync(f, 'utf8').matchAll(/registerSettingsPane\(\{\s*id:\s*'([^']+)'[^}]*?order:\s*(\d+)/g)) {
      count++;
      const order = Number(m[2]);
      assert.equal(seen.get(order), undefined, `pane "${m[1]}" shares order ${order} with "${seen.get(order)}"`);
      seen.set(order, m[1]!);
    }
  }
  assert.ok(count >= 10, `found only ${count} pane registrations; the pattern no longer matches the source`);
});
