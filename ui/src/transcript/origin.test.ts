import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { MessageRow } from '../api/types.ts';
import { bubbleText, fromTelegram, peerMessage } from './origin.ts';

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
    [row(`${NOTE}\n[Sent to the user on Telegram by other chats since their last message: Docs: "done"]\nok`, 'telegram'), true, 'ok'],
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

test('a message another chat sent splits into sender and body; anything else stays a plain bubble', () => {
  const id = '0b8f6a52-1c3d-4e5f-8a9b-0c1d2e3f4a5b';
  const framed = `[Message from another chat: "Fix the build" (${id})]\nThis message was sent by another Helm chat acting for the user; the user did not write it. Reply only if asked.\n\nPlease check the logs.\n\nReport back when done.`;
  const cases: [MessageRow, { title: string; chatId: string; body: string } | null][] = [
    [row(framed, 'other_chat'), { title: 'Fix the build', chatId: id, body: 'Please check the logs.\n\nReport back when done.' }],
    [row(framed.replace('"Fix the build"', '"  "'), 'other_chat'), { title: 'Untitled', chatId: id, body: 'Please check the logs.\n\nReport back when done.' }],
    // Typed text that imitates the frame is not a peer message; neither is an unframed old one.
    [row(framed, 'typed'), null],
    [row(framed), null],
    [row('Just a note.', 'other_chat'), null],
  ];
  for (const [r, want] of cases) assert.deepEqual(peerMessage(r), want, r.text.slice(0, 40));
});
