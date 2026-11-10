import { test } from 'node:test';
import assert from 'node:assert/strict';
import { agoText, compactCount, contextSize, messageTimeLabel, contextTokens, formatTurnTime, isTooLargeRefusal, newTokens, tokenChip, turnTimes, skillsByReply, chatRefOpens } from './rowMeta.ts';

const usage = { inputTokens: 1200, outputTokens: 300, cacheReadTokens: 40_000, cacheWriteTokens: 500, costUsd: 0 };

test('turn time reads in seconds, minutes and hours', () => {
  assert.equal(formatTurnTime(400), 'less than a second');
  assert.equal(formatTurnTime(12_400), '12s');
  // Rounded like formatElapsed, so a 3.6 s turn reads "4s" in both places.
  assert.equal(formatTurnTime(3_600), '4s');
  assert.equal(formatTurnTime(134_000), '2m 14s');
  assert.equal(formatTurnTime(3_780_000), '1h 3m');
});

test('the token chip leaves out input read from the cache', () => {
  assert.equal(newTokens(usage), 2000);
  const chip = tokenChip(usage);
  assert.equal(chip.text, '2k tokens');
  assert.match(chip.title, /40,000\) is not counted/);
});

test('compact counts', () => {
  assert.equal(compactCount(950), '950');
  assert.equal(compactCount(1250), '1.3k');
  assert.equal(compactCount(12_400), '12k');
  assert.equal(compactCount(3_400_000), '3.4M');
});

test('the turn time sits on the last reply of a turn whose work folded', async () => {
  const { layout } = await import('./workBlocks.ts');
  const { row } = await import('../dev/fixtures.ts');
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'let me look', timestamp: 5_000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 6_000 }),
    row({ id: 4, role: 'assistant', text: 'done', timestamp: 20_000 }),
    row({ id: 5, role: 'user', text: 'Hello', timestamp: 30_000 }),
    row({ id: 6, role: 'assistant', text: 'Hi', timestamp: 60_000 }),
  ];
  const times = turnTimes(layout(rows, false));
  assert.deepEqual([...times], [[4, 15_000]]);
});

test('the context size is the newest reply that reports usage: input plus cache reads and writes', async () => {
  const { row } = await import('../dev/fixtures.ts');
  const usage = (inputTokens: number, cacheReadTokens: number, cacheWriteTokens: number) => ({ inputTokens, cacheReadTokens, cacheWriteTokens, outputTokens: 5, costUsd: 0 });
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'assistant', details: { usage: usage(10, 90_000, 500) } }),
    row({ id: 3, role: 'assistant', details: { usage: usage(20, 120_000, 1_000) } }),
    row({ id: 4, role: 'assistant' }),
  ];
  assert.equal(contextTokens(rows), 121_020);
  assert.equal(contextTokens(rows.slice(0, 1)), null);
  assert.equal(contextTokens([]), null);
});

test('the shown context size follows the last measured call and the model\'s real window', async () => {
  const { row } = await import('../dev/fixtures.ts');
  // (rows' own newest reply, host reading, expected tokens/window/percent)
  const host = (contextTokens: number | null, windowTokens: number | null) => ({ contextTokens, windowTokens });
  const cases: [number, ReturnType<typeof host> | null, number, number | null, number | null][] = [
    // The host's last measured turn beats a row that stopped reporting (the feedback case:
    // a row said 126k while the chat's real last call held 230k of a 262k window).
    [126_000, host(230_000, 262_144), 230_000, 262_144, 87.74],
    // A streaming reply measures on its row before the turn's usage entry lands.
    [240_000, host(230_000, 262_144), 240_000, 262_144, 91.55],
    // No host reading: the rows' own reading, with no percent claimed.
    [126_000, null, 126_000, null, null],
    // A server that has not stated a window: the token count, no percent.
    [126_000, host(230_000, null), 230_000, null, null],
    // Over the window is stated as over 100, not clipped.
    [300_000, host(300_000, 262_144), 300_000, 262_144, 114.44],
  ];
  for (const [rowTokens, h, tokens, window, pct] of cases) {
    const rows = [
      row({ id: 1, role: 'user' }),
      row({
        id: 2,
        role: 'assistant',
        details: { usage: { inputTokens: rowTokens, cacheReadTokens: 0, cacheWriteTokens: 0, outputTokens: 5, costUsd: 0 } },
      }),
    ];
    const size = contextSize(rows, h);
    assert.deepEqual([size?.tokens, size?.window, size?.pct], [tokens, window, pct], JSON.stringify(h));
  }
  // Nothing measured anywhere: nothing to show.
  assert.equal(contextSize([], null), null);
  assert.equal(contextSize([], host(null, null)), null);
});

test('a too-large-for-the-window refusal is recognised; other faults and no error are not', () => {
  for (const e of ['Prompt is too long', 'prompt is too long: 300000 tokens > 262144 maximum', 'Context length exceeded', 'maximum context length is 8192 tokens', 'Too Many Tokens']) {
    assert.equal(isTooLargeRefusal(e), true, e);
  }
  for (const e of [null, undefined, '', "You've hit your weekly limit", 'Connection error.', 'claude did not settle after Stop']) {
    assert.equal(isTooLargeRefusal(e), false, String(e));
  }
});

test('a message tooltip names the day and time and how long ago, and the age reads in plain words', () => {
  const at = (y: number, m: number, d: number, h: number, min: number) => new Date(y, m - 1, d, h, min).getTime();
  const now = at(2026, 11, 10, 15, 41);
  for (const [ms, want] of [
    [0, 'just now'],
    [30_000, 'just now'],
    [60_000, '1 minute ago'],
    [5 * 60_000, '5 minutes ago'],
    [80 * 60_000, '80 minutes ago'],
    [3 * 3_600_000, '3 hours ago'],
    [2 * 86_400_000, '2 days ago'],
    [-5000, 'just now'],
  ] as const) {
    assert.equal(agoText(ms), want, String(ms));
  }
  for (const [stamp, start, end] of [
    [at(2026, 11, 10, 15, 36), 'Today at ', ' · 5 minutes ago'],
    [at(2026, 11, 9, 23, 30), 'Yesterday at ', ' · 16 hours ago'],
    [at(2026, 11, 7, 9, 0), '', ' · 3 days ago'],
    [at(2026, 10, 1, 9, 0), '', ' · 40 days ago'],
  ] as const) {
    const label = messageTimeLabel(stamp, now);
    assert.ok(label.startsWith(start) && label.endsWith(end), label);
    assert.match(label, / at .*\d/, label);
  }
});

test('the skills of a turn sit on its last reply, once each; chat chips open only for chats the list holds', async () => {
  const { row } = await import('../dev/fixtures.ts');
  const skill = (id: number, name: string) => row({ id, role: 'toolResult', toolName: 'Skill', details: { toolArgs: JSON.stringify({ skill: name }) } });
  const rows = [
    row({ id: 1, role: 'user' }),
    skill(2, 'personal:spanish-trainer'),
    row({ id: 3, role: 'assistant', text: 'one' }),
    skill(4, 'helm:review'),
    skill(5, 'personal:spanish-trainer'),
    row({ id: 6, role: 'assistant', text: 'two' }),
    row({ id: 7, role: 'user' }),
    row({ id: 8, role: 'toolResult', toolName: 'Read' }),
    row({ id: 9, role: 'assistant', text: 'plain' }),
    row({ id: 10, role: 'user' }),
    skill(11, 'project:deploy'),
    row({ id: 12, role: 'assistant', text: '' }),
  ];
  assert.deepEqual([...skillsByReply(rows)], [[6, 'personal:spanish-trainer\nhelm:review']]);
  assert.equal(chatRefOpens([], 'x'), true, 'a list that has not loaded says nothing');
  assert.equal(chatRefOpens([{ id: 'a' }], 'a'), true);
  assert.equal(chatRefOpens([{ id: 'a' }], 'gone'), false);
});

