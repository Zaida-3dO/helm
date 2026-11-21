import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULT_MODEL, looksLikeToken, modelOptions, repoSummary, sourceLabel, statusLine, tokenRow, turnOptions } from './model.ts';

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
  const repo = (paths: string[]) => ({ repoId: 'r', worktreesAllowed: true, locations: paths.map((path, i) => ({ id: String(i), path })) });
  assert.equal(repoSummary(repo([])), 'No checkout listed');
  assert.equal(repoSummary(repo(['C:/a'])), 'C:/a');
  assert.equal(repoSummary(repo(['C:/a', 'C:/b', 'C:/c'])), 'C:/a and 2 more');
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

test('the sub-agent tool-call picker lists the choices in order and keeps an unlisted stored value', () => {
  assert.deepEqual(turnOptions(null).map((o) => o.value), ['40', '60', '100', '150', '250', '500']);
  assert.deepEqual(turnOptions(120).map((o) => o.value), ['40', '60', '100', '120', '150', '250', '500']);
});

test('the sign-in row shows no token, a saved token, one near its year, and one past it; pasted text is checked', () => {
  const day = 86_400_000;
  const saved = Date.UTC(2026, 10, 21, 12);
  assert.equal(tokenRow(null, saved).state, 'checking');
  const none = tokenRow({ saved: false }, saved);
  assert.equal(none.state, 'none');
  assert.equal(none.title, "Chats use this PC's Claude sign-in");

  const ok = tokenRow({ saved: true, savedAt: saved }, saved + 30 * day);
  assert.equal(ok.state, 'saved');
  assert.equal(ok.title, 'Chats sign in with a saved Claude token (saved 21 Nov 2026)');
  // 14 days before the year is up it starts to warn; at the year it says it has run out.
  assert.equal(tokenRow({ saved: true, savedAt: saved }, saved + 350 * day).state, 'saved');
  const soon = tokenRow({ saved: true, savedAt: saved }, saved + 351 * day);
  assert.equal(soon.state, 'expiring');
  assert.match(soon.note, /runs out in 14 days/);
  assert.match(tokenRow({ saved: true, savedAt: saved }, saved + 364 * day).note, /in 1 day\./);
  assert.equal(tokenRow({ saved: true, savedAt: saved }, saved + 365 * day).state, 'expired');

  const good = 'sk-ant-oat01-AbC123_def-GHI456jklMNO789';
  assert.equal(looksLikeToken(`  ${good}\n`), true);
  for (const bad of ['', 'sk-ant-short', 'sk-ant-api03-AbC123_def-GHI456jklMNO789', `${good} extra`, 'hello world']) {
    assert.equal(looksLikeToken(bad), false, bad);
  }
});
