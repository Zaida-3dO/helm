// The page bridge's wire rules, as plain functions: what a frame may send, how big, and how a
// message from a page is worded when it reaches a chat. No DOM here, so the rules are testable.

export const MARK = 'helm-page-v1';
/** Requests a page may have waiting at once. */
export const MAX_PENDING = 8;
/** Largest request, serialised. */
export const MAX_PAYLOAD = 300 * 1024;
export const MAX_TEXT = 16_000;
export const MAX_TITLE = 240;
/** Largest `data` of a message, pretty-printed. */
export const MAX_DATA = 64_000;
export const MAX_LINK = 4096;
export const MAX_ERROR = 1000;
export const MAX_STATE = 256 * 1024;
/** The only things a page may ask the app for. */
export const METHODS = ['setState', 'sendMessage', 'openLink'] as const;
export type Method = (typeof METHODS)[number];

export type PageMessage =
  | { type: 'ready' }
  | { type: 'request'; id: number; method: string; payload: unknown }
  | { type: 'error'; message: string }
  | { type: 'networkBlocked'; uri: string };

/** A message from a frame, if it is well formed and carries this channel; otherwise null. */
export function parseMessage(data: unknown, channel: string): PageMessage | null {
  if (!data || typeof data !== 'object') return null;
  const m = data as Record<string, unknown>;
  if (m.mark !== MARK || m.channel !== channel) return null;
  switch (m.type) {
    case 'ready':
      return { type: 'ready' };
    case 'request':
      if (typeof m.id !== 'number' || !Number.isSafeInteger(m.id) || m.id < 1) return null;
      if (typeof m.method !== 'string' || m.method.length > 32) return null;
      return { type: 'request', id: m.id, method: m.method, payload: m.payload };
    case 'error':
      return { type: 'error', message: typeof m.message === 'string' ? m.message.slice(0, MAX_ERROR) : 'Script error' };
    case 'networkBlocked':
      return { type: 'networkBlocked', uri: typeof m.uri === 'string' ? m.uri.slice(0, 200) : '' };
    default:
      return null;
  }
}

export function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

export type Checked<T> = { ok: true; value: T } | { ok: false; error: string };

export interface Handoff {
  text: string;
  title?: string;
  data?: unknown;
}

/** A `sendMessage` request: text 1 to 16,000 characters, a title of at most 240, JSON data of at most 64,000. */
export function checkHandoff(payload: unknown): Checked<Handoff> {
  if (!payload || typeof payload !== 'object') return { ok: false, error: 'Unsupported app request.' };
  const p = payload as Record<string, unknown>;
  const text = typeof p.text === 'string' ? p.text.trim() : '';
  if (!text || text.length > MAX_TEXT) return { ok: false, error: 'The message text is not valid.' };
  const out: Handoff = { text };
  if (p.title !== undefined) {
    if (typeof p.title !== 'string' || p.title.length > MAX_TITLE) return { ok: false, error: 'The message title is not valid.' };
    if (p.title.trim()) out.title = p.title.trim();
  }
  if (p.data !== undefined) {
    let pretty: string | undefined;
    try {
      pretty = JSON.stringify(p.data, null, 2);
    } catch {
      pretty = undefined;
    }
    if (pretty === undefined || pretty.length > MAX_DATA) return { ok: false, error: 'The message data is not valid.' };
    out.data = p.data;
  }
  return { ok: true, value: out };
}

/** What the confirmation dialog is titled. */
export function handoffTitle(h: Handoff, pageTitle: string): string {
  return h.title ?? `From ${pageTitle}`;
}

/**
 * The text that reaches the chat: the page's words, a blank line, a line naming the page (so it
 * never reads as the user's own typing) and, when the page attached data, that data as JSON.
 */
export function composeHandoff(h: Handoff, pageTitle: string): string {
  const name = pageTitle.replace(/["\r\n]+/g, ' ').trim() || 'a page';
  let out = `${h.text}\n\n[Sent from the page "${name}" in Helm's side panel. The page wrote this, not the user.]`;
  if (h.data !== undefined) out += `\n\nSelected page data:\n\`\`\`json\n${JSON.stringify(h.data, null, 2)}\n\`\`\``;
  return out;
}

/** Hosts that are Helm's own or this machine's, which a page's link may never name. */
const OWN_HOST = /(^|\.)localhost$|^127\.|^\[?::1\]?$/i;

/** A link a page asks to open: a plain web address that is not one of Helm's own. */
export function checkLink(payload: unknown): Checked<string> {
  const raw = payload && typeof payload === 'object' ? (payload as { url?: unknown }).url : undefined;
  if (typeof raw !== 'string' || !raw || raw.length > MAX_LINK) return { ok: false, error: 'The link is not valid.' };
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, error: 'The link is not valid.' };
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return { ok: false, error: 'Only web links can be opened.' };
  if (url.username || url.password) return { ok: false, error: 'The link is not valid.' };
  if (OWN_HOST.test(url.hostname)) return { ok: false, error: 'The link is not valid.' };
  return { ok: true, value: url.href };
}

/** A `setState` request: the state and the revision it was last saved at. */
export function checkState(payload: unknown): Checked<{ state: unknown; revision: number }> {
  if (!payload || typeof payload !== 'object') return { ok: false, error: 'Unsupported app request.' };
  const p = payload as { state?: unknown; revision?: unknown };
  if (typeof p.revision !== 'number' || !Number.isSafeInteger(p.revision) || p.revision < 0) {
    return { ok: false, error: 'Unsupported app request.' };
  }
  let text: string | undefined;
  try {
    text = JSON.stringify(p.state);
  } catch {
    text = undefined;
  }
  if (text === undefined) return { ok: false, error: 'Expected JSON state.' };
  if (byteLength(text) > MAX_STATE) return { ok: false, error: 'App state exceeds 256 KiB.' };
  return { ok: true, value: { state: p.state, revision: p.revision } };
}

/**
 * The frame's sandbox. It leaves out `allow-same-origin`, which is what gives the page an opaque
 * origin: no access to the app's page, cookies or storage. It also leaves out downloads (a page
 * could otherwise drop files on the PC without asking), top navigation, popups, forms, modals and
 * pointer lock.
 */
export const PAGE_SANDBOX = 'allow-scripts';

/**
 * The sandbox for one open: {@link PAGE_SANDBOX}, or nothing allowed at all when the host says the
 * page's scripts may not run (offline, in a web view that cannot keep them off the network).
 */
export function pageSandbox(scripts: boolean): string {
  return scripts ? PAGE_SANDBOX : '';
}
