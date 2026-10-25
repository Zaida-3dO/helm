// A fake Telegram Bot API. It answers the methods the Helm hub's bot
// calls, under `/bot<token>/<method>` with JSON bodies, in the Bot API's `{ok, result}` shape:
// getMe, getUpdates (long poll with offset and timeout), sendMessage, editMessageText,
// editMessageReplyMarkup, deleteMessage, answerCallbackQuery and getFile, plus file downloads
// under `/file/bot<token>/<file_path>`. A wrong token is answered 401 like the real API. Tests
// inject incoming messages, voice notes and button taps, and read back everything the bot sent:
//
//   POST /__fake/message  {from, text, chatId?}  queue an incoming message (an update)
//   GET  /__fake/sent                            every send, edit, delete and tap answer, in order
//   GET  /__fake/chat/<chatId>                   the chat as a phone would show it now
//
// `startFakeStt` is a tiny OpenAI-compatible speech-to-text endpoint for voice notes: it reads
// the multipart upload and answers a fixed transcript as plain text.
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
export const FAKE_TRANSCRIPT = 'This is a fake transcript.';
const MAX_TEXT = 4096;

export interface SentCall {
  method: 'sendMessage' | 'editMessageText' | 'editMessageReplyMarkup' | 'deleteMessage' | 'answerCallbackQuery';
  /** For answerCallbackQuery: the chat and message whose button was tapped. */
  chatId: number;
  messageId: number;
  text?: string;
  silent?: boolean;
  /** sendMessage and the edits: the inline keyboard sent. An edit without one removes it. */
  replyMarkup?: unknown;
  /** answerCallbackQuery: the tap it answers. */
  callbackQueryId?: string;
}

export interface ChatMessage {
  messageId: number;
  text: string;
  edits: number;
  /** The inline keyboard it carries now, if any. */
  replyMarkup?: unknown;
}

export interface FakeTelegram {
  url: string;
  token: string;
  /** Queue an incoming message from `from` (a numeric user id) in its private chat. */
  message(from: number, text: string, chatId?: number): void;
  /** Queue a voice note from `from`; getFile and the file download then serve `bytes`. */
  voice(from: number, bytes: Uint8Array, chatId?: number): void;
  /**
   * Queue a picture from `from`: a photo (listed at two sizes) or, with `as: 'file'`, an image sent
   * as a document of type `mime`. getFile and the file download then serve `bytes`.
   */
  picture(from: number, bytes: Uint8Array, opts?: { caption?: string; as?: 'photo' | 'file'; mime?: string; chatId?: number }): void;
  /** Queue a tap on a button (`data`, its callback data) of the bot's message `messageId`. */
  tap(from: number, messageId: number, data: string, chatId?: number): void;
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
  let nextFile = 1;
  const sent: SentCall[] = [];
  const chats = new Map<number, ChatMessage[]>();
  const files = new Map<string, { path: string; bytes: Uint8Array }>();
  const queries = new Map<string, { chatId: number; messageId: number; answered: boolean }>();
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

  const push = (update: Json) => {
    updates.push({ update_id: nextUpdate++, ...update });
    wake();
  };
  const incoming = (from: number, chatId: number, fields: Json) => ({
    message_id: nextMessage++,
    date: Math.floor(Date.now() / 1000),
    from: { id: from, is_bot: false, first_name: 'User' },
    chat: { id: chatId, type: chatId === from ? 'private' : 'group' },
    ...fields,
  });

  const inject = (from: number, text: string, chatId = from) => push({ message: incoming(from, chatId, { text }) });

  const injectVoice = (from: number, bytes: Uint8Array, chatId = from) => {
    const n = nextFile++;
    const fileId = `fake-file-${n}`;
    files.set(fileId, { path: `voice/file_${n}.oga`, bytes });
    const voice = { file_id: fileId, file_unique_id: `u${n}`, duration: 1, mime_type: 'audio/ogg', file_size: bytes.length };
    push({ message: incoming(from, chatId, { voice }) });
  };

  const injectPicture = (
    from: number,
    bytes: Uint8Array,
    { caption, as = 'photo', mime = 'image/jpeg', chatId = from }: { caption?: string; as?: 'photo' | 'file'; mime?: string; chatId?: number } = {},
  ) => {
    const n = nextFile++;
    const fileId = `fake-file-${n}`;
    files.set(fileId, { path: `photos/file_${n}`, bytes });
    const kind =
      as === 'photo'
        ? {
            photo: [
              { file_id: `${fileId}-small`, file_unique_id: `s${n}`, width: 90, height: 90, file_size: 800 },
              { file_id: fileId, file_unique_id: `u${n}`, width: 1280, height: 1280, file_size: bytes.length },
            ],
          }
        : { document: { file_id: fileId, file_unique_id: `u${n}`, file_name: `picture_${n}`, mime_type: mime, file_size: bytes.length } };
    files.set(`${fileId}-small`, { path: `photos/file_${n}_small`, bytes });
    push({ message: incoming(from, chatId, { ...kind, ...(caption === undefined ? {} : { caption }) }) });
  };

  const injectTap = (from: number, messageId: number, data: string, chatId = from) => {
    const id = `fake-query-${nextUpdate}`;
    queries.set(id, { chatId, messageId, answered: false });
    const text = chatOf(chatId).find((m) => m.messageId === messageId)?.text ?? '';
    push({
      callback_query: {
        id,
        from: { id: from, is_bot: false, first_name: 'User' },
        message: { message_id: messageId, date: Math.floor(Date.now() / 1000), text, chat: { id: chatId, type: chatId === from ? 'private' : 'group' } },
        chat_instance: 'fake-instance',
        data,
      },
    });
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
        const replyMarkup = body.reply_markup;
        chatOf(chatId).push({ messageId, text, edits: 0, replyMarkup });
        sent.push({ method: 'sendMessage', chatId, messageId, text, silent: body.disable_notification === true, replyMarkup });
        return ok(res, { message_id: messageId, chat: { id: chatId }, text });
      }
      case 'editMessageText': {
        const messageId = Number(body.message_id);
        const m = chatOf(chatId).find((x) => x.messageId === messageId);
        if (!m) return bad(res, 400, 'Bad Request: message to edit not found');
        const text = String(body.text ?? '');
        if (m.text === text) return bad(res, 400, 'Bad Request: message is not modified');
        m.text = text;
        m.replyMarkup = body.reply_markup;
        m.edits++;
        sent.push({ method: 'editMessageText', chatId, messageId, text, replyMarkup: body.reply_markup });
        return ok(res, { message_id: messageId, chat: { id: chatId }, text });
      }
      case 'editMessageReplyMarkup': {
        const messageId = Number(body.message_id);
        const m = chatOf(chatId).find((x) => x.messageId === messageId);
        if (!m) return bad(res, 400, 'Bad Request: message to edit not found');
        m.replyMarkup = body.reply_markup;
        sent.push({ method: 'editMessageReplyMarkup', chatId, messageId, replyMarkup: body.reply_markup });
        return ok(res, { message_id: messageId, chat: { id: chatId }, text: m.text });
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
      case 'answerCallbackQuery': {
        const id = String(body.callback_query_id ?? '');
        const q = queries.get(id);
        if (!q || q.answered) return bad(res, 400, 'Bad Request: query is too old and response timeout expired or query ID is invalid');
        q.answered = true;
        const text = body.text === undefined ? undefined : String(body.text);
        sent.push({ method: 'answerCallbackQuery', chatId: q.chatId, messageId: q.messageId, text, callbackQueryId: id });
        return ok(res, true);
      }
      case 'getFile': {
        const id = String(body.file_id ?? '');
        const f = files.get(id);
        if (!f) return bad(res, 400, 'Bad Request: invalid file_id');
        return ok(res, { file_id: id, file_unique_id: `u-${id}`, file_size: f.bytes.length, file_path: f.path });
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
    const download = /^\/file\/bot([^/]+)\/(.+)$/.exec(url.pathname);
    if (download) {
      if (download[1] !== token) {
        unauthorized++;
        return bad(res, 401, 'Unauthorized');
      }
      const f = [...files.values()].find((x) => x.path === download[2]);
      if (!f) return bad(res, 404, 'Not Found');
      res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': f.bytes.length });
      return res.end(f.bytes);
    }
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
    voice: (from, bytes, chatId) => injectVoice(from, bytes, chatId),
    picture: (from, bytes, opts) => injectPicture(from, bytes, opts),
    tap: (from, messageId, data, chatId) => injectTap(from, messageId, data, chatId),
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

export interface SttUpload {
  /** The plain form fields (model, language, response_format). */
  fields: Record<string, string>;
  fileName?: string;
  fileBytes: number;
}

export interface FakeStt {
  /** The transcriptions endpoint: point HUB_TELEGRAM_STT_URL here. */
  url: string;
  uploads: SttUpload[];
  close(): Promise<void>;
}

/** A fake OpenAI-compatible speech-to-text server: every upload is answered 200 with `transcript`. */
export async function startFakeStt(opts: { port?: number; transcript?: string } = {}): Promise<FakeStt> {
  const transcript = opts.transcript ?? FAKE_TRANSCRIPT;
  const uploads: SttUpload[] = [];
  const server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => {
      const boundary = /boundary=([^;]+)/.exec(req.headers['content-type'] ?? '')?.[1];
      if (req.method !== 'POST' || new URL(req.url ?? '/', 'http://fake').pathname !== '/v1/audio/transcriptions' || !boundary) {
        res.writeHead(404, { 'content-type': 'text/plain' });
        return res.end('Not Found');
      }
      const body = Buffer.concat(chunks);
      const upload: SttUpload = { fields: {}, fileBytes: 0 };
      const delimiter = Buffer.from(`--${boundary}`);
      let at = body.indexOf(delimiter);
      while (at >= 0) {
        const next = body.indexOf(delimiter, at + delimiter.length);
        if (next < 0) break;
        const part = body.subarray(at + delimiter.length + 2, next - 2); // past CRLF, before CRLF
        const split = part.indexOf('\r\n\r\n');
        const head = part.subarray(0, split).toString('utf8');
        const value = part.subarray(split + 4);
        const name = /name="([^"]*)"/.exec(head)?.[1] ?? '';
        const fileName = /filename="([^"]*)"/.exec(head)?.[1];
        if (fileName !== undefined) {
          upload.fileName = fileName;
          upload.fileBytes = value.length;
        } else {
          upload.fields[name] = value.toString('utf8');
        }
        at = next;
      }
      uploads.push(upload);
      res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' });
      res.end(transcript);
    });
  });
  await new Promise<void>((resolve) => server.listen(opts.port ?? 0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}/v1/audio/transcriptions`,
    uploads,
    close: () =>
      new Promise<void>((resolve) => {
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
