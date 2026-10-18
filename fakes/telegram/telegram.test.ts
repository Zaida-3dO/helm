import assert from 'node:assert/strict';
import { test } from 'node:test';
import { BOT_USERNAME, DEFAULT_TOKEN, startFakeTelegram } from './server.ts';

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
