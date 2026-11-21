// Link targets: what a clicked link means to the app, and which hrefs are safe to emit at all.
//
// The renderer never navigates. Every anchor it produces carries its target in `data-md-*`
// attributes, and the component turns a click into an `onLink(target)` call (index.ts contract).
// Only chat links also carry their target as `href` (the canonical `helm://chats/...`), so a link
// copied out of the app still names the chat; every other kind keeps a neutral `#`.

import { driveForm, isRemoteOrDevicePath } from './paths.ts';
import { hasExtension } from './pathrefs.ts';

export interface LinkTarget {
  /** `path`: a file path written in inline code or plain text, resolved by the host on click.
   *  `helm`: one of Helm's own file, page or panel links (see {@link parseHelmLink}). */
  kind: 'url' | 'vault' | 'path' | 'chat' | 'helm';
  /** `url`: the href as written (http, https, mailto, file, or a plain path). `path`: the path as written, with any line suffix. `chat` and `helm`: the link as written. */
  href?: string;
  /** `vault` only: the note name or path, without `.md`. Empty means the current note. */
  note?: string;
  heading?: string;
  block?: string;
  alias?: string;
  /** `vault` only: the vault an `obsidian://open?vault=` link names. Absent means "resolve it". */
  vault?: string;
}

const IMAGE_EXTENSIONS = /\.(png|jpe?g|gif|svg|webp|bmp|avif)$/i;

export function isImageName(name: string): boolean {
  return IMAGE_EXTENSIONS.test(name);
}

const CHAT_LINK = /^helm:\/\/chats\/([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})(?:\/messages\/(\d{1,15}))?\/?$/;

/**
 * A chat link: `helm://chats/<id>`, optionally `/messages/<number>`. The id is read case-insensitively
 * and comes back lowercase, and one trailing slash is tolerated, because a hand-written or copied
 * link often has one. Everything else stays strict: no user info, port, query, fragment, doubled
 * slash or extra segments.
 */
export function parseChatLink(href: string): { chatId: string; messageId?: number } | null {
  const m = CHAT_LINK.exec(href);
  if (!m) return null;
  const chatId = (m[1] as string).toLowerCase();
  return m[2] === undefined ? { chatId } : { chatId, messageId: Number(m[2]) };
}

/** The panel views a `helm://panel/<view>` link may show. */
export const PANEL_VIEWS = ['changes', 'files', 'tasks'] as const;
export type PanelView = (typeof PANEL_VIEWS)[number];

/** One of Helm's own links, read and checked. */
export type HelmLink =
  | { form: 'chat'; chatId: string; messageId?: number }
  /** A file the host resolves against the chat (so it works for a chat that runs on another PC). */
  | { form: 'file'; path: string; line?: number; column?: number }
  /** A pinned page, by its pin id. */
  | { form: 'page'; pageId: string }
  | { form: 'panel'; view: PanelView };

/** The longest decoded path a file link may carry. */
const MAX_LINK_PATH = 4096;
/** The highest line or column a file link may name. */
const MAX_LINE = 10_000_000;

// The encoded path: URL path characters and percent escapes only. No query, fragment, whitespace or
// raw backslash (a backslash is written %5C). A raw "/" and ":" are allowed, so "C:/src/a.ts" reads.
const FILE_LINK = /^helm:\/\/files\/([A-Za-z0-9\-._~%!$&'()*+,;=@:/]+)$/;
const PAGE_LINK = /^helm:\/\/pages\/([A-Za-z0-9-]{1,64})$/;
const PANEL_LINK = /^helm:\/\/panel\/([a-z]+)$/;
// eslint-disable-next-line no-control-regex
const CONTROL_CHARS = /[\u0000-\u001f\u007f-\u009f]/;

/**
 * The decoded path of a file link, with its line and column, or null when it must not open. The
 * line suffix is read before decoding, so an encoded colon stays part of the name; the decoded path
 * is then refused when it holds a control character, names a network share or a device path (in
 * any spelling, double-encoded included), is drive-relative ("C:x") or starts with a scheme.
 */
function fileLinkPath(encoded: string): { path: string; line?: number; column?: number } | null {
  let raw = encoded;
  let line: number | undefined;
  let column: number | undefined;
  const m = /:(\d{1,8})(?::(\d{1,8}))?$/.exec(raw);
  // "C:5" alone is a drive-relative path, not a path with a line.
  if (m && m.index > 1) {
    line = Number(m[1]);
    column = m[2] === undefined ? undefined : Number(m[2]);
    if (!(line >= 1 && line <= MAX_LINE) || (column !== undefined && !(column >= 1 && column <= MAX_LINE))) return null;
    raw = raw.slice(0, m.index);
  }
  let path: string;
  try {
    path = decodeURIComponent(raw);
  } catch {
    return null;
  }
  if (!path.trim() || path.length > MAX_LINK_PATH || CONTROL_CHARS.test(path)) return null;
  if (isRemoteOrDevicePath(path)) return null;
  const drive = driveForm(path);
  if (drive === 'relative') return null;
  // A path is a path: "https:", "file:" or "javascript:" inside a file link is never followed.
  if (!drive && /^[a-z][a-z0-9+.-]+:/i.test(path)) return null;
  const out: { path: string; line?: number; column?: number } = { path };
  if (line !== undefined) out.line = line;
  if (column !== undefined) out.column = column;
  return out;
}

/**
 * Helm's own links, each in one strict form (no user info, port, query, fragment or trailing
 * slash), or null for anything else:
 *
 *   helm://chats/<id>[/messages/<n>]           a chat
 *   helm://files/<percent-encoded path>[:line[:col]]   a file, resolved against the chat
 *   helm://pages/<pin id>                     a pinned page
 *   helm://panel/changes | files | tasks      a view of the side panel
 */
export function parseHelmLink(href: string): HelmLink | null {
  if (typeof href !== 'string' || href.length > 8 * 1024) return null;
  const chat = parseChatLink(href);
  if (chat) return { form: 'chat', ...chat };
  let m = FILE_LINK.exec(href);
  if (m) {
    const file = fileLinkPath(m[1] as string);
    return file ? { form: 'file', ...file } : null;
  }
  m = PAGE_LINK.exec(href);
  if (m) return { form: 'page', pageId: m[1] as string };
  m = PANEL_LINK.exec(href);
  if (m && (PANEL_VIEWS as readonly string[]).includes(m[1] as string)) return { form: 'panel', view: m[1] as PanelView };
  return null;
}

/** The file link for a path (and line), in the form {@link parseHelmLink} reads back. */
export function helmFileHref(path: string, line?: number): string {
  const enc = encodeURIComponent(path).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
  return `helm://files/${enc}${line !== undefined && line >= 1 ? `:${Math.floor(line)}` : ''}`;
}

type Parts = Pick<LinkTarget, 'note' | 'heading' | 'block'>;

/** Splits `Note#Heading`, `Note#^block` and `#Heading` into their parts. */
function splitNoteAndFragment(raw: string): Parts {
  const hash = raw.indexOf('#');
  const out: Parts = { note: (hash < 0 ? raw : raw.slice(0, hash)).trim() };
  if (hash < 0) return out;
  const fragment = raw.slice(hash + 1).trim();
  const blockAt = fragment.indexOf('#^');
  if (fragment.startsWith('^')) {
    out.block = fragment.slice(1).trim();
  } else if (blockAt >= 0) {
    out.heading = fragment.slice(0, blockAt).trim();
    out.block = fragment.slice(blockAt + 2).trim();
  } else {
    out.heading = fragment;
  }
  if (!out.heading) delete out.heading;
  if (!out.block) delete out.block;
  return out;
}

/** The inside of `[[...]]`: `note#heading|alias`. Null when it names nothing. */
export function parseWikilink(inner: string): LinkTarget | null {
  const text = inner.replace(/\\\|/g, '|');
  const bar = text.indexOf('|');
  const targetPart = bar < 0 ? text : text.slice(0, bar);
  const alias = bar < 0 ? '' : text.slice(bar + 1).trim();
  const parts = splitNoteAndFragment(targetPart);
  if (!parts.note && !parts.heading && !parts.block) return null;
  // A wikilink naming a network share or a device path stays text: never a link or an embed.
  if (parts.note && isRemoteOrDevicePath(parts.note)) return null;
  const target: LinkTarget = { kind: 'vault', ...parts };
  if (alias) target.alias = alias;
  return target;
}

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/**
 * Classifies a Markdown link destination. Returns null when it must not become a link
 * (javascript:, data:, vbscript:, protocol-relative, UNC and device paths, the drive-relative
 * `C:x` form, anything outside the allowlist). The panel's link gate re-checks every target.
 */
export function classifyHref(rawHref: string): LinkTarget | null {
  // Control characters inside the scheme are how `java\tscript:` gets past naive filters.
  // eslint-disable-next-line no-control-regex
  const href = rawHref.replace(/[\u0000-\u001f\u007f]/g, '').trim();
  if (!href) return null;
  if (href.startsWith('//')) return null;
  if (isRemoteOrDevicePath(href)) return null;

  if (href.startsWith('#')) {
    return { kind: 'vault', ...splitNoteAndFragment(safeDecode(href)) };
  }

  // A single letter is a Windows drive only when a slash follows ("C:\notes\x.md"). "C:x" is
  // drive-relative, and treating any one-letter scheme as a drive let "C:javascript:..." through.
  const drive = driveForm(href);
  if (drive === 'relative') return null;
  const scheme = drive ? null : /^([a-z][a-z0-9+.-]*):/i.exec(href);
  if (scheme) {
    const name = (scheme[1] as string).toLowerCase();
    if (name === 'http' || name === 'https' || name === 'mailto' || name === 'file') {
      return { kind: 'url', href };
    }
    if (name === 'obsidian') return classifyObsidianUri(href);
    if (name === 'helm') {
      const link = parseHelmLink(href);
      return !link ? null : link.form === 'chat' ? { kind: 'chat', href } : { kind: 'helm', href };
    }
    return null;
  }

  // A relative or drive path. Markdown notes are vault targets; anything else is a file path
  // the handler may decide to open.
  const hash = href.indexOf('#');
  const pathPart = safeDecode(hash < 0 ? href : href.slice(0, hash));
  // A bare word ("income") names no folder and no kind of file: a model's loose wording, not a path.
  // It stays text instead of becoming a link that can only miss.
  if (!/[\\/]/.test(pathPart) && !hasExtension(pathPart)) return null;
  if (/\.md$/i.test(pathPart)) {
    const fragment = hash < 0 ? '' : safeDecode(href.slice(hash));
    return { kind: 'vault', ...splitNoteAndFragment(pathPart.replace(/\.md$/i, '') + fragment) };
  }
  return { kind: 'url', href };
}

/** `obsidian://open?vault=V&file=Folder%2FNote` becomes a vault target. */
function classifyObsidianUri(href: string): LinkTarget | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  if (url.hostname !== 'open') return null;
  const file = url.searchParams.get('file');
  if (!file || isRemoteOrDevicePath(file)) return null;
  const target: LinkTarget = { kind: 'vault', ...splitNoteAndFragment(file.replace(/\.md$/i, '')) };
  const vault = url.searchParams.get('vault');
  if (vault) target.vault = vault;
  return target;
}

/** The `data-md-*` attributes that carry a target through the HTML string. */
export function targetAttrs(target: LinkTarget): Array<[string, string]> {
  const attrs: Array<[string, string]> = [['data-md-kind', target.kind]];
  if (target.href !== undefined) attrs.push(['data-md-href', target.href]);
  if (target.note !== undefined) attrs.push(['data-md-note', target.note]);
  if (target.heading) attrs.push(['data-md-heading', target.heading]);
  if (target.block) attrs.push(['data-md-block', target.block]);
  if (target.alias) attrs.push(['data-md-alias', target.alias]);
  if (target.vault) attrs.push(['data-md-vault', target.vault]);
  return attrs;
}

/** Reads a target back from the attributes written by {@link targetAttrs}. */
export function targetFromAttrs(get: (name: string) => string | null): LinkTarget | null {
  const kind = get('data-md-kind');
  if (kind !== 'url' && kind !== 'vault' && kind !== 'path' && kind !== 'chat' && kind !== 'helm') return null;
  const target: LinkTarget = { kind };
  const href = get('data-md-href');
  const note = get('data-md-note');
  const heading = get('data-md-heading');
  const block = get('data-md-block');
  const alias = get('data-md-alias');
  const vault = get('data-md-vault');
  if (href !== null) target.href = href;
  if (note !== null) target.note = note;
  if (heading !== null) target.heading = heading;
  if (block !== null) target.block = block;
  if (alias !== null) target.alias = alias;
  if (vault !== null) target.vault = vault;
  return target;
}

/** The slice of a click event the link delegation reads (a React mouse event fits it). */
export interface ClickLike {
  target: unknown;
  currentTarget: { contains(node: unknown): boolean };
  preventDefault(): void;
}

/**
 * The target of a click on a rendered link inside `currentTarget`, or null when the click was not
 * on one. A link click is always cancelled, so the webview never navigates by itself.
 */
export function linkFromClick(event: ClickLike): LinkTarget | null {
  const anchor = (event.target as Element).closest<HTMLElement>('a[data-md-kind]');
  if (!anchor || !event.currentTarget.contains(anchor)) return null;
  event.preventDefault();
  return targetFromAttrs((name) => anchor.getAttribute(name));
}
