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
