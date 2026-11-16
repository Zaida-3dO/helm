import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { GateAsk } from './types.ts';
import {
  agoText,
  barNote,
  canChangeAnswer,
  canReview,
  channelLabel,
  detailRows,
  frequencyLabel,
  gateLine,
  reviewLabel,
  reviewLine,
  stateLabel,
  stopCheckLine,
  stopClass,
  stopKindLabel,
  tookText,
  verdictLabel,
} from './model.ts';

const T = 1_763_000_000_000; // a fixed "now"
const MIN = 60_000;

const ask = (over: Partial<GateAsk> = {}): GateAsk => ({
  id: 'a1',
  profileId: 'p',
  chatId: 'c1',
  chatTitle: 'Notes app cleanup',
  channel: 'question',
  requestRef: 'r1',
  askedAt: T - 2 * 60 * MIN,
  question: 'Which export format?',
  verdict: 'decided',
  state: 'open',
  review: 'pending',
  ...over,
});

test('the review list and card lines: wording per verdict, state, channel and mark', () => {
  for (const [got, want] of [
    [agoText(T, T - 30_000), 'just now'],
    [agoText(T, T - 5 * MIN), '5 min ago'],
    [agoText(T, T - 2 * 60 * MIN), '2 h ago'],
    [agoText(T, T - 9 * 24 * 60 * MIN), '9 d ago'],
    [agoText(T, T - 1000), 'just now'], // a small clock skew reads as just now, never negative
  ] as const)
    assert.equal(got, want);
  assert.equal(tookText(2100), '2.1 s');
  assert.equal(tookText(450), '450 ms');
  assert.equal(tookText(null), null);
  assert.equal(tookText(undefined), null);

  assert.equal(gateLine('checking'), 'Checking whether this needs you…');
  assert.equal(gateLine('failed'), "Helm couldn't check this one.");
  assert.equal(gateLine('failed', 'The judgement ran out of time.'), 'The judgement ran out of time.');
  assert.equal(gateLine('failed', '   '), "Helm couldn't check this one.");
  assert.equal(gateLine('escalated'), null);
  assert.equal(gateLine('decided'), null);

  for (const [got, want] of [
    [verdictLabel('checking'), 'Checking'],
    [verdictLabel('decided'), 'Decided for you'],
    [verdictLabel('escalated'), 'Asked you'],
    [verdictLabel('failed'), "Couldn't check"],
    [verdictLabel('passed'), 'Left for you'],
    [channelLabel('telegram_ask'), 'Telegram ask'],
    [channelLabel('board_pick'), 'Board pick'],
    [channelLabel('plan'), 'Plan'],
    [stateLabel('parked'), 'Waiting for your answer'],
    [stateLabel('kept'), 'You left it'],
    [stateLabel('vetoed'), 'You changed the answer'],
    [stateLabel('open'), null],
    [reviewLabel('fine'), 'Fine'],
    [reviewLabel('should_have_asked'), 'Should have asked me'],
    [reviewLabel('didnt_need_me'), "Didn't need me"],
    [reviewLabel('pending'), null],
    [frequencyLabel('less'), 'Less'],
    [frequencyLabel('balanced'), 'Balanced'],
    [frequencyLabel('more'), 'More'],
    [channelLabel('stop'), 'Stop'],
    [stopCheckLine(), 'Checking whether this stop needs you…'],
    [stopKindLabel('stopBlocker'), 'Blocker'],
    [stopKindLabel('stopLimit'), 'Plan limit'],
    [stopKindLabel('stopFailure'), 'Failure'],
    [stopKindLabel('stopAsked'), 'Stopped to ask'],
    [stopKindLabel('gatekeeper'), null],
    [stopClass('stopBlocker'), 'blocker'],
    [stopClass('stopLimit'), 'limit'],
    [stopClass('stopFailure'), 'failure'],
    [stopClass('stopAsked'), 'asked'],
  ] as const)
    assert.equal(got, want);

  assert.equal(
    reviewLine(ask({ choice: 'CSV', reason: 'it opens in a spreadsheet' })),
    'Helm chose «CSV» for “Which export format?” because it opens in a spreadsheet.',
  );
  assert.equal(reviewLine(ask({ choice: 'CSV' })), 'Helm chose «CSV» for “Which export format?”.');
  assert.equal(reviewLine(ask({ choice: undefined })), 'Helm chose an answer for “Which export format?”.');

  assert.equal(barNote({ askFrequency: 'balanced', bar: 0.75 }), 'Helm answers safe questions when it is at least 75% sure.');
  assert.match(barNote({ askFrequency: 'less', bar: 0.6 }), /comes to you rarely/);
  assert.match(barNote({ askFrequency: 'more', bar: 0.9 }), /comes to you more often/);
});

test('what stands on a row: marks on decided and answered-asked rows only while pending; Change answer only on an unsettled decided row', () => {
  for (const [a, review, change] of [
    [ask(), true, true],
    [ask({ review: 'fine' }), false, false],
    [ask({ state: 'kept' }), true, true], // a kept row still accepts a mark or a change
    [ask({ state: 'vetoed' }), false, false],
    [ask({ verdict: 'escalated', state: 'answered' }), true, false], // "didn't need me" marks an answered ask
    [ask({ verdict: 'failed' }), false, false],
    [ask({ verdict: 'passed' }), false, false],
    [ask({ verdict: 'decided', state: 'answered' }), false, false],
  ] as const) {
    assert.equal(canReview(a), review, `canReview ${a.verdict}/${a.state}/${a.review}`);
    assert.equal(canChangeAnswer(a), change, `canChangeAnswer ${a.verdict}/${a.state}/${a.review}`);
  }
});

test('the History detail lists only the facts that are there, in reading order', () => {
  const rows = detailRows(
    ask({
      tookMs: 2100,
      model: 'sonnet',
      confidence: 0.9,
      risk: 'low',
      reason: 'it opens in a spreadsheet',
      choice: 'CSV',
      answer: 'No, use JSON',
      answeredVia: 'helm',
      review: 'should_have_asked',
      reviewNote: 'I read these in a text editor',
    }),
  );
  const labels = rows.map((r) => r.label);
  assert.deepEqual(labels, ['Asked', 'Judged in', 'Judged by', 'Confidence', 'Risk', 'Because', 'Chose', 'Answer', 'Answered', 'Your mark']);
  assert.equal(rows.find((r) => r.label === 'Confidence')!.value, '90%');
  assert.equal(rows.find((r) => r.label === 'Risk')!.value, 'Low');
  assert.equal(rows.find((r) => r.label === 'Chose')!.value, '«CSV»');
  assert.equal(rows.find((r) => r.label === 'Answered')!.value, 'From Helm');
  assert.equal(rows.find((r) => r.label === 'Your mark')!.value, 'Should have asked me — I read these in a text editor');

  const bare = detailRows(ask({ tookMs: null }));
  assert.deepEqual(
    bare.map((r) => r.label),
    ['Asked'],
  );
  const parked = detailRows(ask({ verdict: 'escalated', state: 'parked' }));
  assert.ok(parked.some((r) => r.label === 'Status' && r.value === 'Waiting for your answer'));
});
