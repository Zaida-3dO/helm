import assert from 'node:assert/strict';
import { test } from 'node:test';
import { BOT_USERNAME, DEFAULT_TOKEN, FAKE_TRANSCRIPT, startFakeStt, startFakeTelegram } from './server.ts';

const call = async (base: string, token: string, method: string, body: unknown = {}) => {
  const r = await fetch(`${base}/bot${token}/${method}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: r.status, body: (await r.json()) as { ok: boolean; result?: any; description?: string } };
};

test('the fake answers the bot methods in the Bot API shape behind the token', async () => {
  const tg = await startFakeTelegram();
  try {
    const me = await call(tg.url, DEFAULT_TOKEN, 'getMe');
    assert.equal(me.body.result.username, BOT_USERNAME);
    const wrong = await call(tg.url, 'nope', 'getMe');
    assert.equal(wrong.status, 401);
    assert.equal(wrong.body.ok, false);
    assert.equal(tg.unauthorized, 1);
  } finally {
    await tg.close();
  }
});

test('getUpdates long-polls for injected messages and honours the offset', async () => {
  const tg = await startFakeTelegram();
  try {
    const poll = call(tg.url, DEFAULT_TOKEN, 'getUpdates', { offset: 0, timeout: 5 });
    setTimeout(() => tg.message(42, 'hello'), 50);
    const u = (await poll).body.result as any[];
    assert.equal(u.length, 1);
    assert.equal(u[0].message.text, 'hello');
    assert.equal(u[0].message.from.id, 42);
    assert.equal(u[0].message.chat.type, 'private');
    const after = await call(tg.url, DEFAULT_TOKEN, 'getUpdates', { offset: u[0].update_id + 1, timeout: 0 });
    assert.deepEqual(after.body.result, []);
  } finally {
    await tg.close();
  }
});

test('send, edit and delete change the chat as a phone would show it', async () => {
  const tg = await startFakeTelegram();
  try {
    const s = await call(tg.url, DEFAULT_TOKEN, 'sendMessage', { chat_id: 42, text: 'Working', disable_notification: true });
    const id = s.body.result.message_id;
    await call(tg.url, DEFAULT_TOKEN, 'editMessageText', { chat_id: 42, message_id: id, text: 'Running tests' });
    const same = await call(tg.url, DEFAULT_TOKEN, 'editMessageText', { chat_id: 42, message_id: id, text: 'Running tests' });
    assert.equal(same.status, 400, 'an identical edit is refused like the real API');
    await call(tg.url, DEFAULT_TOKEN, 'sendMessage', { chat_id: 42, text: 'Done' });
    await call(tg.url, DEFAULT_TOKEN, 'deleteMessage', { chat_id: 42, message_id: id });
    assert.deepEqual(tg.chat(42).map((m) => m.text), ['Done']);
    assert.deepEqual(tg.sent.map((c) => c.method), ['sendMessage', 'editMessageText', 'sendMessage', 'deleteMessage']);
    assert.equal(tg.sent[0]!.silent, true);
    const long = await call(tg.url, DEFAULT_TOKEN, 'sendMessage', { chat_id: 42, text: 'x'.repeat(4097) });
    assert.equal(long.status, 400);
  } finally {
    await tg.close();
  }
});

test('voice notes download and transcribe; buttons are tapped, answered and removed', async () => {
  const [tg, stt] = await Promise.all([startFakeTelegram(), startFakeStt()]);
  try {
    tg.voice(42, new Uint8Array([79, 103, 103, 83]));
    const keyboard = { inline_keyboard: [[{ text: 'Allow', callback_data: 'a:0123456789abcdef:0' }]] };
    const ask = await call(tg.url, DEFAULT_TOKEN, 'sendMessage', { chat_id: 42, text: 'Run it?', reply_markup: keyboard });
    tg.tap(42, ask.body.result.message_id, 'a:0123456789abcdef:0');
    const [note, tap] = (await call(tg.url, DEFAULT_TOKEN, 'getUpdates', { offset: 0 })).body.result as any[];
    const file = (await call(tg.url, DEFAULT_TOKEN, 'getFile', { file_id: note.message.voice.file_id })).body.result;
    assert.equal(file.file_size, 4);
    const audio = await fetch(`${tg.url}/file/bot${DEFAULT_TOKEN}/${file.file_path}`);
    assert.deepEqual(new Uint8Array(await audio.arrayBuffer()), new Uint8Array([79, 103, 103, 83]));
    assert.equal((await fetch(`${tg.url}/file/botnope/${file.file_path}`)).status, 401);
    const form = new FormData();
    form.append('file', new Blob([new Uint8Array([1, 2, 3])]), 'file_1.oga');
    form.append('model', 'large-v3-turbo');
    const heard = await fetch(stt.url, { method: 'POST', body: form });
    assert.equal(await heard.text(), FAKE_TRANSCRIPT);
    assert.deepEqual(stt.uploads, [{ fields: { model: 'large-v3-turbo' }, fileName: 'file_1.oga', fileBytes: 3 }]);
    assert.equal(tap.callback_query.data, 'a:0123456789abcdef:0');
    assert.equal(tap.callback_query.message.chat.type, 'private');
    const answer = { callback_query_id: tap.callback_query.id, text: 'Sent to Helm' };
    assert.equal((await call(tg.url, DEFAULT_TOKEN, 'answerCallbackQuery', answer)).status, 200);
    assert.equal((await call(tg.url, DEFAULT_TOKEN, 'answerCallbackQuery', answer)).status, 400, 'answered once');
    assert.deepEqual(tg.chat(42)[0]!.replyMarkup, keyboard);
    await call(tg.url, DEFAULT_TOKEN, 'editMessageText', { chat_id: 42, message_id: ask.body.result.message_id, text: 'Run it?\n\n→ Allow' });
    assert.equal(tg.chat(42)[0]!.replyMarkup, undefined, 'an edit without a keyboard removes it');
    assert.deepEqual(tg.sent.map((c) => c.method), ['sendMessage', 'answerCallbackQuery', 'editMessageText']);
    assert.equal(tg.sent[1]!.messageId, ask.body.result.message_id);
  } finally {
    await Promise.all([tg.close(), stt.close()]);
  }
});
