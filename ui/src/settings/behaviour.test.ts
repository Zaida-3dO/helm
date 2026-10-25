import { test } from 'node:test';
import assert from 'node:assert/strict';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import { parseSendBehavior, sendBehaviorDiff, waitLimitLabel } from './chat/sendBehavior.ts';
import { changedBy, changeText, describeValue, keyTitle, parseChanges } from './changed/model.ts';
import { parseNameList } from './applied.ts';
import { stepZoom } from './generalOptions.ts';

test('send behaviour reads a stored difference and writes back only what differs from the defaults', () => {
  assert.deepEqual(parseSendBehavior(null), HOME_SEND_BEHAVIOR);
  assert.deepEqual(parseSendBehavior({ mode: 'sideways', waitLimitSeconds: 2, toolStop: 1 }), HOME_SEND_BEHAVIOR);
  const custom = parseSendBehavior({ mode: 'interrupt', waitLimitSeconds: null, toolStop: false });
  assert.equal(custom.mode, 'interrupt');
  assert.equal(custom.waitLimitSeconds, null);
  assert.deepEqual(sendBehaviorDiff(custom), { mode: 'interrupt', waitLimitSeconds: null, toolStop: false });
  // "Queue it" is gone: a stored queue choice reads as the default and is dropped on the next save.
  const old = parseSendBehavior({ mode: 'queue', toolStop: false });
  assert.deepEqual(sendBehaviorDiff(old), { toolStop: false });
  assert.deepEqual(sendBehaviorDiff(HOME_SEND_BEHAVIOR), {});
  assert.equal(waitLimitLabel(null), 'No limit');
  assert.equal(waitLimitLabel(30), '30 seconds (default)');
  assert.equal(waitLimitLabel(120), '2 minutes');
});

test('changes by chats are read leniently and described in plain words', () => {
  const list = parseChanges({
    changes: [
      { changeId: 3, chatId: 'c', key: 'compactSidebar', before: null, after: true, at: 5, undone: false },
      { changeId: 'x', key: 'bad' },
    ],
  });
  assert.equal(list.length, 1);
  assert.equal(changeText(list[0]!), 'was the default, now on');
  assert.equal(describeValue({ a: 'x'.repeat(100) }).length, 60);
  assert.equal(keyTitle('compactSidebar'), 'Compact sidebar');
  assert.deepEqual(parseChanges(undefined), []);
  const chats = [{ id: 'c', title: 'Fix\u202e it\n' + 'x'.repeat(80) }, { id: 'd', title: ' ' }];
  const who = changedBy('c', chats);
  assert.ok(who.known && who.text.startsWith('Fix it') && !who.text.includes('\u202e') && who.text.length === 64);
  assert.deepEqual(changedBy('d', chats), { text: 'an untitled chat', known: true });
  assert.deepEqual(changedBy('gone', chats), { text: 'a chat that is gone', known: false });
});

test('name lists and zoom steps stay in range', () => {
  assert.deepEqual(parseNameList([' a ', 'a', '', 5, 'b']), ['a', 'b']);
  assert.deepEqual(parseNameList('a'), []);
  assert.equal(stepZoom(100, 1), 110);
  assert.equal(stepZoom(200, 1), 200);
  assert.equal(stepZoom(50, -1), 50);
});
