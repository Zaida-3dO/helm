import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { MessageRow } from '../api/types.ts';
import { bubbleText, fromSpeaker, fromTelegram, fromVoice, isHelmNote, isQuietReply, pageMessage, peerMessage, typedOn } from './origin.ts';

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

test('a room speaker message shows without its channel line and the pointer to the talk before', () => {
  const where = '[Speaker · study / Study · speak it, short, no markup]\n';
  const cases: [MessageRow, string][] = [
    [row(`${where}what time is it`, 'speaker'), 'what time is it'],
    [row(`${where}[Previous conversation ended: HA speaker kitchen voice 2027-01-15 — It is 7:20.]\ngo on`, 'speaker'), 'go on'],
    [row('[Speaker · the house · speak it, short, no markup]\nhi', 'typed'), '[Speaker · the house · speak it, short, no markup]\nhi'],
    [row('a plain speaker message', 'speaker'), 'a plain speaker message'],
  ];
  for (const [r, shown] of cases) {
    assert.equal(bubbleText(r), shown, r.text);
    assert.equal(fromSpeaker(r), r.details?.origin === 'speaker');
  }
});

test('the line naming the PC a message was typed on is hidden only for a message from another PC', () => {
  const user = (text: string, origin?: MessageRow['details']['origin']) => row(text, origin);
  const cases: [MessageRow, string | null, string][] = [
    [user('[Typed on Laptop]\nhello', 'remote'), 'Laptop', 'hello'],
    [user('[Typed on Desk upstairs]\nline one\n[Typed on X]\nkept', 'remote'), 'Desk upstairs', 'line one\n[Typed on X]\nkept'],
    [user(`[Typed on ${'x'.repeat(40)}]\nok`, 'remote'), 'x'.repeat(40), 'ok'],
    // 40 characters counted as characters, not UTF-16 units: a name of emoji is still stripped.
    [user(`[Typed on ${String.fromCodePoint(0x1f5a5).repeat(40)}]\nok`, 'remote'), String.fromCodePoint(0x1f5a5).repeat(40), 'ok'],
    // Not that kind of message, or not exactly that line: shown as written.
    [user('[Typed on Laptop]\nhello', 'typed'), null, '[Typed on Laptop]\nhello'],
    [user('[Typed on Laptop]\nhello'), null, '[Typed on Laptop]\nhello'],
    [user('[Typed on Laptop]\nhello', 'telegram'), null, '[Typed on Laptop]\nhello'],
    [user('hello [Typed on Laptop]\n', 'remote'), null, 'hello [Typed on Laptop]\n'],
    [user('[Typed on Laptop] hello', 'remote'), null, '[Typed on Laptop] hello'],
    [user(`[Typed on ${'x'.repeat(41)}]\nlong`, 'remote'), null, `[Typed on ${'x'.repeat(41)}]\nlong`],
    [user('[Typed on La[p]top]\nx', 'remote'), null, '[Typed on La[p]top]\nx'],
    [user(`[Typed on La${String.fromCharCode(9)}ptop]\nx`, 'remote'), null, `[Typed on La${String.fromCharCode(9)}ptop]\nx`],
    [user('[Typed on  ]\nx', 'remote'), null, '[Typed on  ]\nx'],
    [user('plain words from the other PC', 'remote'), null, 'plain words from the other PC'],
  ];
  for (const [r, pc, shown] of cases) {
    assert.equal(typedOn(r), pc, r.text);
    assert.equal(bubbleText(r), shown, r.text);
  }
  assert.equal(typedOn({ ...row('[Typed on Laptop]\nx', 'remote'), role: 'assistant' }), null);
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

test('a message a page wrote carries its card words; the origin alone makes it one', () => {
  const withWords = {
    ...row('Approve these five elements\n\n[Sent from the page "Approvals" in Helm\'s side panel. The page wrote this, not the user.]', 'page'),
    details: { origin: 'page' as const, pageTitle: 'Approvals', pageOverview: '- header\n- footer' },
  };
  assert.deepEqual(pageMessage(withWords), { title: 'Approvals', overview: '- header\n- footer' });
  // An old or odd page row without the words still gets a card; the renderer fills in the name.
  const bare = row('a page note', 'page');
  assert.deepEqual(pageMessage(bare), { title: '', overview: '' });
  // Not that kind of message at all.
  assert.equal(pageMessage(row('from a page, but typed', 'typed')), null);
  assert.equal(pageMessage(row('a peer note', 'other_chat')), null);
  assert.equal(pageMessage({ ...row('page words', 'page'), role: 'assistant' }), null);
});

test('the notes Helm writes for the agent are recognised; nothing a person or another chat wrote is', () => {
  assert.equal(isHelmNote(row('[Attention] Build: Waiting for an approval\n', 'app')), true);
  assert.equal(isHelmNote(row('[Wait finished] The build is done.', 'app')), true);
  assert.equal(isHelmNote(row('[Attention] typed by hand')), false, 'a typed message is never hidden');
  assert.equal(isHelmNote(row('[Attention] from the phone', 'telegram')), false);
  assert.equal(isHelmNote(row('Something else', 'app')), false);
  // A voice chat's "[quiet]" answer is a signal, however it is spaced or cased; only the agent's.
  const said = (text: string) => ({ ...row(text), role: 'assistant' as const });
  const quiet: [MessageRow, boolean][] = [
    [said('[quiet]'), true],
    [said(' [Quiet]\n'), true],
    [said('[ quiet ]'), true],
    [said('[quiet] but here is more'), false],
    [said('quiet'), false],
    [row('[quiet]'), false],
  ];
  for (const [r, want] of quiet) assert.equal(isQuietReply(r), want, JSON.stringify(r.text));
});

