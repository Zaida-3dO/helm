import assert from 'node:assert/strict';
import { test } from 'node:test';
import { busyLine, busyLines, installProblem, restartBody } from './model.ts';

test('the restart dialog lists at most four busy chats, cuts long titles and says what is kept', () => {
  const long = 'x'.repeat(100);
  assert.equal(busyLine(long).length, 60);
  assert.ok(busyLine(long).endsWith('…'));
  assert.equal(busyLine('short (C:/p)'), 'short (C:/p)');
  const rows: [string[], number, string[]][] = [
    [[], 0, []],
    [['a', 'b'], 2, ['a', 'b']],
    [['a', 'b', 'c', 'd'], 4, ['a', 'b', 'c', 'd']],
    [['a', 'b', 'c', 'd', 'e'], 7, ['a', 'b', 'c', 'd', 'and 3 more']],
  ];
  for (const [busy, busyTotal, want] of rows) assert.deepEqual(busyLines({ busy, busyTotal }), want);
  assert.ok(restartBody({ version: '0.3.1', busyTotal: 0 }).includes('0.3.1'));
  assert.ok(!restartBody({ version: '0.3.1', busyTotal: 0 }).includes('stopped'));
  assert.ok(restartBody({ version: '0.3.1', busyTotal: 2 }).includes('stopped'));
  assert.equal(installProblem('restarting'), null);
  for (const o of ['save_failed', 'busy', 'cancelled'] as const) assert.ok(installProblem(o));
});
