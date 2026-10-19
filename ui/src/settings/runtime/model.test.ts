import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULT_MODEL, modelOptions, sourceLabel, statusLine } from './model.ts';

test('a found component shows its version and where it came from; a missing one shows why', () => {
  assert.deepEqual(statusLine({ path: 'C:/node/node.exe', source: 'PATH', version: 'v24.14.0', problem: null }), {
    ready: true,
    text: 'v24.14.0, found on PATH',
  });
  assert.deepEqual(statusLine({ path: null, source: null, version: null, problem: 'Node.js was not found.' }), {
    ready: false,
    text: 'Node.js was not found.',
  });
  const quiet = statusLine({ path: 'C:/c/claude.exe', source: 'local-bin', version: null, problem: 'did not answer --version.' });
  assert.equal(quiet.ready, false);
  assert.equal(quiet.text, 'version unknown, in ~/.local/bin: did not answer --version.');
  assert.equal(sourceLabel('something-new'), 'something-new');
});

test("the model picker starts with Claude Code's default and keeps a stored model it no longer lists", () => {
  const opts = modelOptions(
    [
      { id: 'default', name: 'Default' },
      { id: 'opus', name: 'Opus', description: 'Most capable' },
      { id: 'opus', name: 'Opus again' },
    ],
    'retired-model',
  );
  assert.deepEqual(
    opts.map((o) => o.value),
    [DEFAULT_MODEL, 'opus', 'retired-model'],
  );
  assert.equal(opts[1]?.subtitle, 'Most capable');
});
