import test from 'node:test';
import assert from 'node:assert/strict';
import { freshAnswer, freshSummary, SUMMARY_DELAY_MS, SUMMARY_POLLS, SUMMARY_POLL_MS } from './summary.ts';

const row = (over: Partial<{ summary: string | null; summaryAt: number | null; lastActivityAt: number; updatedAt: number }> = {}) => ({
  summary: null,
  summaryAt: null,
  lastActivityAt: 100,
  updatedAt: 100,
  ...over,
});

test('a row summary shows only while it covers the chat', () => {
  for (const [over, want] of [
    [{}, null],
    [{ summary: 'About the plan. It waits on review.', summaryAt: 120 }, 'About the plan. It waits on review.'],
    [{ summary: 'About the plan. It waits on review.', summaryAt: 100 }, 'About the plan. It waits on review.'],
    // The chat moved after the summary was made: stale until a fresh one arrives.
    [{ summary: 'About the plan. It waits on review.', summaryAt: 99 }, null],
    [{ summary: '  ', summaryAt: 120 }, null],
    [{ summary: 'About the plan. It waits on review.' }, null],
    // No activity stamp: the record's own update stands in, as in the row's age line.
    [{ lastActivityAt: 0, updatedAt: 90, summary: 'Two sentences.', summaryAt: 90 }, 'Two sentences.'],
  ] as const) {
    assert.equal(freshSummary(row(over)), want, JSON.stringify(over));
  }
});

test('an answer from the host is used only when it is fresh and non-empty', () => {
  for (const [a, want] of [
    [{ summary: 'A about B. Stuck on C.', summaryAt: 120, generating: false }, 'A about B. Stuck on C.'],
    [{ summary: 'Old one.', summaryAt: 99, generating: false }, null],
    [{ summary: null, summaryAt: null, generating: true }, null],
    [{ summary: '   ', summaryAt: 120, generating: false }, null],
  ] as const) {
    assert.equal(freshAnswer(a, row()), want);
  }
});

test('the timing rules keep the card quiet', () => {
  // A second of hover before the summary is asked for, and bounded retries after.
  assert.equal(SUMMARY_DELAY_MS, 1_000);
  assert.ok(SUMMARY_POLL_MS >= 500 && SUMMARY_POLLS * SUMMARY_POLL_MS <= 12_000);
});
