import { test } from 'node:test';
import assert from 'node:assert/strict';
import { compactCount, contextTokens, formatTurnTime, newTokens, tokenChip, turnTimes } from './rowMeta.ts';

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
