import assert from 'node:assert/strict';
import { test } from 'node:test';
import { panelStore, registerPanelKind } from './registry.ts';
import { startTabSync } from './sync.ts';

const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

test('saved tabs come back as program intent, nothing is written before the read, then only changes are saved', async () => {
  const off = registerPanelKind({
    kind: 'synctest',
    title: 't',
    render: () => null,
    persist: { version: 1, save: (t) => t.data, restore: (d) => d },
  });
  const saves: unknown[] = [];
  let release: (v: unknown) => void = () => undefined;
  const stop = startTabSync(
    {
      tabsGet: () => new Promise((r) => (release = r)),
      tabsSave: async (v) => {
        saves.push(v);
        return null;
      },
    },
    10,
  );
  try {
    // A change before the read settles must not write.
    panelStore.changed();
    await tick(30);
    assert.equal(saves.length, 0);

    release({ version: 1, sets: { c9: [{ kind: 'synctest', key: 'k', v: 1, data: { a: 1 } }] } });
    await tick(5);
    const tab = panelStore.model.tabs.find((t) => t.kind === 'synctest');
    assert.ok(tab);
    assert.equal(tab.intent, 'program');
    assert.equal(tab.set, 'c9');

    // An unrelated change that leaves the value the same writes nothing.
    panelStore.changed();
    await tick(30);
    assert.equal(saves.length, 0);

    panelStore.update((m) => m.open('synctest', 'k2', { b: 2 }, { intent: 'user', chatId: 'c9' }, null));
    await tick(30);
    assert.equal(saves.length, 1);
    assert.deepEqual((saves[0] as { sets: Record<string, unknown[]> }).sets.c9?.length, 2);
  } finally {
    stop();
    panelStore.update((m) => m.dropSet('c9'));
    off();
  }
});
