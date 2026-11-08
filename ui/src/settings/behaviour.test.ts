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
  // Retired choices read as the default and drop out on the next save: the "queue" send mode
  // and the stop-sends-waiting switch.
  const old = parseSendBehavior({ mode: 'queue', stopSendsWaiting: true, toolStop: false });
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
  // A chat's plain-http opt-in for a private-network server reads as a sentence, not as JSON.
  const optIn = (before: unknown, after: unknown) => ({ changeId: 1, chatId: 'c', key: 'mcpUnencryptedServers', before, after, at: 1, undone: false });
  assert.equal(changeText(optIn({}, { nas: { url: 'http://192.168.1.5:8080/mcp', unencrypted: true } })), 'Allowed plain http for nas (http://192.168.1.5:8080/mcp)');
  assert.equal(
    changeText(optIn({ nas: { url: 'http://10.0.0.2/mcp', unencrypted: true } }, { nas: { url: 'http://10.0.0.3/mcp', unencrypted: true } })),
    'Moved nas from http://10.0.0.2/mcp to http://10.0.0.3/mcp, still plain http',
  );
  assert.equal(keyTitle('mcpUnencryptedServers'), 'Plain http for MCP servers');
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
