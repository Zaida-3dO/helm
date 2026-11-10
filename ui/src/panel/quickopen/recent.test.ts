import assert from 'node:assert/strict';
import { test } from 'node:test';
import { addRecent, MAX_RECENT, recentPaths } from './recent.ts';

test('recent paths are newest first, de-duplicated case-insensitively and capped', () => {
  addRecent('C:\\a\\one.ts');
  addRecent('C:\\a\\two.ts');
  addRecent('c:\\A\\ONE.ts');
  assert.deepEqual(recentPaths().slice(0, 2), ['c:\\A\\ONE.ts', 'C:\\a\\two.ts']);
  for (let i = 0; i < 20; i++) addRecent(`C:\\x\\${i}.ts`);
  assert.equal(recentPaths().length, MAX_RECENT);
  assert.equal(recentPaths()[0], 'C:\\x\\19.ts');
});

test('two clean Shift taps close together open Go to file; typing capitals and slow taps do not', async () => {
  const { DoubleShift, parseDoubleShift, DOUBLE_SHIFT_MS } = await import('./doubleShift.ts');
  const shift = { key: 'Shift' };
  // Each step is [event, key, time]; `hits` are the times at which a double tap completes.
  const run = (steps: Array<['down' | 'up', { key: string; repeat?: boolean; ctrlKey?: boolean }, number]>) => {
    const d = new DoubleShift();
    const hits: number[] = [];
    for (const [kind, e, t] of steps) {
      if (kind === 'down') d.down(e);
      else if (d.up(e, t)) hits.push(t);
    }
    return hits;
  };
  for (const [name, steps, want] of [
    ['two quick taps', [['down', shift, 0], ['up', shift, 50], ['down', shift, 150], ['up', shift, 200]], [200]],
    ['taps too far apart', [['down', shift, 0], ['up', shift, 50], ['down', shift, 100 + DOUBLE_SHIFT_MS], ['up', shift, 150 + DOUBLE_SHIFT_MS]], []],
    ['a capital letter between', [['down', shift, 0], ['down', { key: 'A' }, 10], ['up', { key: 'A' }, 20], ['up', shift, 30], ['down', shift, 100], ['up', shift, 140]], []],
    ['a letter between two taps', [['down', shift, 0], ['up', shift, 40], ['down', { key: 'a' }, 60], ['up', { key: 'a' }, 80], ['down', shift, 100], ['up', shift, 140]], []],
    ['held with key repeat counts once', [['down', shift, 0], ['down', { key: 'Shift', repeat: true }, 30], ['up', shift, 60]], []],
    ['Shift with Ctrl is not a tap', [['down', { key: 'Shift', ctrlKey: true }, 0], ['up', shift, 40], ['down', shift, 80], ['up', shift, 120]], []],
    ['three taps open once, then start over', [['down', shift, 0], ['up', shift, 40], ['down', shift, 80], ['up', shift, 120], ['down', shift, 160], ['up', shift, 200]], [120]],
  ] as const) {
    assert.deepEqual(run(steps as never), want, name);
  }
  for (const [stored, on] of [[null, true], [undefined, true], [true, true], [false, false], [0, true]] as const) {
    assert.equal(parseDoubleShift(stored), on, String(stored));
  }
});
