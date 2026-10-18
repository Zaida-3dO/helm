// A fake Telegram Bot API. It answers the methods the Helm hub's bot
// calls, under `/bot<token>/<method>` with JSON bodies, in the Bot API's `{ok, result}` shape:
// getMe, getUpdates (long poll with offset and timeout), sendMessage, editMessageText and
// deleteMessage. A wrong token is answered 401 like the real API. Tests inject incoming messages
// and read back everything the bot sent:
//
//   POST /__fake/message  {from, text, chatId?}  queue an incoming message (an update)
//   GET  /__fake/sent                            every sendMessage/edit/delete, in order
//   GET  /__fake/chat/<chatId>                   the chat as a phone would show it now
//
// CLI: `node fakes/telegram/server.ts [--port N] [--token T]` prints
// `fake-telegram listening http://127.0.0.1:<port>`. Point the hub at it with
// HUB_TELEGRAM_API=<that url> and HUB_TELEGRAM_BOT_TOKEN=<the token> (default
// `fake-telegram-token`). Every id here is made up.

import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { fileURLToPath } from 'node:url';

export const DEFAULT_TOKEN = 'fake-telegram-token';
export const BOT_USERNAME = 'helm_fake_bot';
const MAX_TEXT = 4096;

export interface SentCall {
  method: 'sendMessage' | 'editMessageText' | 'deleteMessage';
  chatId: number;
  messageId: number;
  text?: string;
  silent?: boolean;
}

export interface ChatMessage {
  messageId: number;
  text: string;
  edits: number;
}

export interface FakeTelegram {
  url: string;
  token: string;
  /** Queue an incoming message from `from` (a numeric user id) in its private chat. */
  message(from: number, text: string, chatId?: number): void;
  sent: SentCall[];
  /** What the bot's messages in a chat look like now (deleted ones gone, edits applied). */
  chat(chatId: number): ChatMessage[];
  /** Requests with a wrong token. */
  unauthorized: number;
  close(): Promise<void>;
}

type Json = Record<string, unknown>;

export async function startFakeTelegram(opts: { port?: number; token?: string } = {}): Promise<FakeTelegram> {
  const token = opts.token ?? DEFAULT_TOKEN;
  const updates: Json[] = [];
  let nextUpdate = 1;
  let nextMessage = 1000;
  const sent: SentCall[] = [];
  const chats = new Map<number, ChatMessage[]>();
  const waiters: (() => void)[] = [];
  let unauthorized = 0;

  const wake = () => {
    for (const w of waiters.splice(0)) w();
  };

  const reply = (res: ServerResponse, status: number, body: Json) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };
  const ok = (res: ServerResponse, result: unknown) => reply(res, 200, { ok: true, result });
  const bad = (res: ServerResponse, code: number, description: string) =>
    reply(res, code, { ok: false, error_code: code, description });

  const readBody = (req: IncomingMessage): Promise<Json> =>
    new Promise((resolve) => {
      let data = '';
      req.on('data', (c: Buffer) => (data += c.toString('utf8')));
      req.on('end', () => {
        try {
          resolve(data ? (JSON.parse(data) as Json) : {});
        } catch {
          resolve({});
        }
      });
    });

  const inject = (from: number, text: string, chatId = from) => {
    updates.push({
      update_id: nextUpdate++,
      message: {
        message_id: nextMessage++,
        date: Math.floor(Date.now() / 1000),
        text,
        from: { id: from, is_bot: false, first_name: 'User' },
        chat: { id: chatId, type: chatId === from ? 'private' : 'group' },
      },
    });
    wake();
  };

  const chatOf = (id: number) => {
    let c = chats.get(id);
    if (!c) chats.set(id, (c = []));
    return c;
  };

  async function method(name: string, body: Json, res: ServerResponse) {
    const chatId = Number(body.chat_id);
    switch (name) {
      case 'getMe':
        return ok(res, { id: 1, is_bot: true, first_name: 'Helm', username: BOT_USERNAME });
      case 'getUpdates': {
        const offset = Number(body.offset ?? 0);
        const timeout = Math.min(Number(body.timeout ?? 0), 50);
        const pending = () => updates.filter((u) => Number(u.update_id) >= offset);
        if (pending().length === 0 && timeout > 0) {
          await new Promise<void>((resolve) => {
            const t = setTimeout(resolve, timeout * 1000);
            waiters.push(() => {
              clearTimeout(t);
              resolve();
            });
          });
        }
        return ok(res, pending());
      }
      case 'sendMessage': {
        const text = String(body.text ?? '');
        if (!text || text.length > MAX_TEXT) return bad(res, 400, 'Bad Request: message text is empty or too long');
        const messageId = nextMessage++;
        chatOf(chatId).push({ messageId, text, edits: 0 });
        sent.push({ method: 'sendMessage', chatId, messageId, text, silent: body.disable_notification === true });
        return ok(res, { message_id: messageId, chat: { id: chatId }, text });
      }
      case 'editMessageText': {
        const messageId = Number(body.message_id);
        const m = chatOf(chatId).find((x) => x.messageId === messageId);
        if (!m) return bad(res, 400, 'Bad Request: message to edit not found');
        const text = String(body.text ?? '');
        if (m.text === text) return bad(res, 400, 'Bad Request: message is not modified');
        m.text = text;
        m.edits++;
        sent.push({ method: 'editMessageText', chatId, messageId, text });
        return ok(res, { message_id: messageId, chat: { id: chatId }, text });
      }
      case 'deleteMessage': {
        const messageId = Number(body.message_id);
        const c = chatOf(chatId);
        const i = c.findIndex((x) => x.messageId === messageId);
        if (i < 0) return bad(res, 400, 'Bad Request: message to delete not found');
        c.splice(i, 1);
        sent.push({ method: 'deleteMessage', chatId, messageId });
        return ok(res, true);
      }
      default:
        return bad(res, 404, 'Not Found');
    }
  }

  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://fake');
    if (url.pathname === '/__fake/message' && req.method === 'POST') {
      const b = await readBody(req);
      const from = Number(b.from);
      inject(from, String(b.text ?? ''), b.chatId === undefined ? from : Number(b.chatId));
      return ok(res, true);
    }
    if (url.pathname === '/__fake/sent') return ok(res, sent);
    const chatMatch = /^\/__fake\/chat\/(-?\d+)$/.exec(url.pathname);
    if (chatMatch) return ok(res, chatOf(Number(chatMatch[1])));
    const m = /^\/bot([^/]+)\/([A-Za-z]+)$/.exec(url.pathname);
    if (!m) return bad(res, 404, 'Not Found');
    if (m[1] !== token) {
      unauthorized++;
      return bad(res, 401, 'Unauthorized');
    }
    if (req.method !== 'POST' && req.method !== 'GET') return bad(res, 405, 'Method Not Allowed');
    await method(m[2]!, await readBody(req), res);
  });

  await new Promise<void>((resolve) => server.listen(opts.port ?? 0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    token,
    message: (from, text, chatId) => inject(from, text, chatId),
    sent,
    chat: (id) => chatOf(id).map((x) => ({ ...x })),
    get unauthorized() {
      return unauthorized;
    },
    close: () =>
      new Promise<void>((resolve) => {
        wake();
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const arg = (k: string) => {
    const i = process.argv.indexOf(k);
    return i >= 0 ? process.argv[i + 1] : undefined;
  };
  const port = arg('--port');
  const f = await startFakeTelegram({ port: port ? Number(port) : undefined, token: arg('--token') });
  console.log(`fake-telegram listening ${f.url}`);
}
