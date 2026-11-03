import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dueLabel, draftFromItem, remindAtFor, savedFromText, STATE_TITLES } from './model.ts';

test('a message or selection becomes a title from its first line and keeps all of it', () => {
  assert.deepEqual(savedFromText('Fix the sign-in copy\nthe wording is stiff'), {
    title: 'Fix the sign-in copy',
    body: 'Fix the sign-in copy\nthe wording is stiff',
  });
  const long = savedFromText(`${'a'.repeat(130)}\nsecond line`);
  assert.equal(long.title.length, 120);
  assert.ok(long.title.endsWith('…'));
  assert.equal(savedFromText('single line').title, 'single line');
  assert.equal(savedFromText('   ').title, 'Chat message');
});

test('the reminder presets name the next wall-clock match, tonight rolling past 9 pm', () => {
  const afternoon = new Date(2026, 10, 3, 14, 30).getTime();
  assert.equal(remindAtFor('hour', afternoon), afternoon + 3_600_000);
  assert.equal(remindAtFor('tonight', afternoon), new Date(2026, 10, 3, 21, 0).getTime());
  const late = new Date(2026, 10, 3, 22, 30).getTime();
  assert.equal(remindAtFor('tonight', late), new Date(2026, 10, 4, 21, 0).getTime(), 'past 9 pm tonight is tomorrow night');
  // Tomorrow morning is the next day's 9 am even early in the morning itself.
  const morning = new Date(2026, 10, 3, 8, 0).getTime();
  assert.equal(remindAtFor('tomorrow', morning), new Date(2026, 10, 4, 9, 0).getTime());
});

test('the due line appears only while a reminder is pending', () => {
  const now = 1_000_000;
  assert.equal(dueLabel({ remindAt: null, remindedAt: null }, now), null);
  assert.equal(dueLabel({ remindAt: now + 45 * 60_000, remindedAt: null }, now), 'Due in 45 min');
  assert.equal(dueLabel({ remindAt: now + 2 * 3_600_000, remindedAt: null }, now), 'Due in 2 h');
  assert.equal(dueLabel({ remindAt: now + 3 * 86_400_000, remindedAt: null }, now), 'Due in 3 d');
  assert.equal(dueLabel({ remindAt: now - 1, remindedAt: null }, now), 'Due now');
  assert.equal(dueLabel({ remindAt: now + 3_600_000, remindedAt: now + 60_000 }, now), null, 'an already-sounded reminder is quiet');
});

test('every state has a plain word, and the draft opens by naming the item', () => {
  assert.deepEqual(Object.keys(STATE_TITLES).sort(), ['dismissed', 'done', 'filed', 'saved']);
  const draft = draftFromItem({ id: 'x', title: 'Pay the invoice', body: 'due on the first' } as never);
  assert.equal(draft, 'Picking this back up:\nPay the invoice\n\ndue on the first');
});
