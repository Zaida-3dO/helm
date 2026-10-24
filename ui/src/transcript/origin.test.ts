import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { MessageRow } from '../api/types.ts';
import { bubbleText, fromTelegram } from './origin.ts';

const NOTE = '[Sent from Telegram: answer briefly in plain text, with no tables or long code. Offer choices as a numbered list. The user can reply "more" for detail.]';

function row(text: string, origin?: MessageRow['details']['origin']): MessageRow {
  return {
    id: 1, chatId: 'c1', sourceKey: 'u:s1', role: 'user', text, thinking: '', thinkingTail: null, toolName: null,
    toolCallId: null, status: 'complete', timestamp: 0, sequence: 0, revision: 0, artifact: null,
    details: origin ? { origin } : {},
  };
}

test('a Telegram message shows without the note for the agent; other messages are left alone', () => {
  const cases: [MessageRow, boolean, string][] = [
    [row(`${NOTE}\nhello`, 'telegram'), true, 'hello'],
    [row(`${NOTE}\n(sent 3 min ago, queued while Helm was offline)\nhi`, 'telegram'), true, '(sent 3 min ago, queued while Helm was offline)\nhi'],
    [row('[Sent from Telegram: x]\nnot from telegram'), false, '[Sent from Telegram: x]\nnot from telegram'],
    [row(`${NOTE}\nscheduled`, 'schedule'), false, `${NOTE}\nscheduled`],
    [row('typed [Sent from Telegram: x]\n', 'telegram'), true, 'typed [Sent from Telegram: x]\n'],
  ];
  for (const [r, tg, shown] of cases) {
    assert.equal(fromTelegram(r), tg, r.text);
    assert.equal(bubbleText(r), shown, r.text);
  }
});
