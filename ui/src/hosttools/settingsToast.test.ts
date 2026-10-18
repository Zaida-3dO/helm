import { test } from 'node:test';
import assert from 'node:assert/strict';
import { noticeFrom, SETTINGS_TOAST_MS, SettingsToastSlot } from './settingsToast.ts';

test('an external_settings event becomes one notice that lasts 8 s', () => {
  let now = 1000;
  const slot = new SettingsToastSlot(() => now);
  slot.show({ kind: 'external_settings', chatId: 'c1', changes: [{ changeId: 4, key: 'theme', before: null, after: 'dark' }] });
  const n = slot.current();
  assert.deepEqual(n?.changeIds, [4]);
  assert.equal(n?.chatId, 'c1');
  assert.equal(n?.text, 'A chat changed a setting: theme');
  now += SETTINGS_TOAST_MS - 1;
  slot.tick();
  assert.ok(slot.current(), 'still up just before 8 s');
  now += 1;
  slot.tick();
  assert.equal(slot.current(), null);
  slot.dismiss();
});

test('many keys are summarised; malformed events show nothing', () => {
  const n = noticeFrom({ chatId: 'c', changes: ['a', 'b', 'c', 'd', 'e'].map((key, i) => ({ changeId: i, key })) }, 0);
  assert.equal(n?.text, 'A chat changed 5 settings: a, b, c and 2 more');
  assert.equal(noticeFrom({ chatId: 'c', changes: [] }, 0), null);
  assert.equal(noticeFrom({ changes: [{ changeId: 1, key: 'x' }] }, 0), null);
  assert.equal(noticeFrom(null, 0), null);
});
