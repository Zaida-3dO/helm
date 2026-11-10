import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { MessageRow } from '../api/types.ts';
import { bubbleText, fromTelegram, fromVoice, isHelmNote, peerMessage } from './origin.ts';

const NOTE = '[Sent from Telegram: answer briefly in plain text, with no tables or long code. Offer choices as a numbered list. Only the final message of each turn reaches the user\'s phone, so make that message stand on its own. The user can reply "more" for detail.]';

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

test('a voice message shows without the primer and notes for the agent; other messages are left alone', () => {
  const primer = '[Voice chat: you are the user\'s voice assistant in Helm.\nEach message says its mode. A message that starts with [Wait finished] is news.\nIf not, answer exactly [quiet].]\n';
  const mode = '[Voice · conversation: speak it, short, no markup]\n';
  const late = '(sent 3 min ago, queued while Helm was offline)\nhi';
  const cases: [MessageRow, string][] = [
    [row(`${primer}${mode}hello`, 'voice'), 'hello'],
    [row(`${mode}hello`, 'voice'), 'hello'],
    [row(`[Voice · chat: read on screen, formatting fine · continuing what they just said]\n[Not heard by the user: your reply "Sure" stopped after sentence 2 of 5]\n${late}`, 'voice'), late],
    [row(`${mode}hello`, 'typed'), `${mode}hello`],
    [row(`${mode}hello`), `${mode}hello`],
    [row('a plain voice message', 'voice'), 'a plain voice message'],
  ];
  for (const [r, shown] of cases) {
    assert.equal(bubbleText(r), shown, r.text);
    assert.equal(fromVoice(r), r.details?.origin === 'voice');
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

test('the notes Helm writes for the agent are recognised; nothing a person or another chat wrote is', () => {
  assert.equal(isHelmNote(row('[Attention] Build: Waiting for an approval\n', 'app')), true);
  assert.equal(isHelmNote(row('[Wait finished] The build is done.', 'app')), true);
  assert.equal(isHelmNote(row('[Attention] typed by hand')), false, 'a typed message is never hidden');
  assert.equal(isHelmNote(row('[Attention] from the phone', 'telegram')), false);
  assert.equal(isHelmNote(row('Something else', 'app')), false);
});

