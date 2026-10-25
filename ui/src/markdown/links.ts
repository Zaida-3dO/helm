// Link targets: what a clicked link means to the app, and which hrefs are safe to emit at all.
//
// The renderer never navigates. Every anchor it produces carries its target in `data-md-*`
// attributes, and the component turns a click into an `onLink(target)` call (index.ts contract).

import { driveForm, isRemoteOrDevicePath } from './paths.ts';

export interface LinkTarget {
  /** `path`: a file path written in inline code or plain text, resolved by the host on click. */
  kind: 'url' | 'vault' | 'path' | 'chat';
  /** `url`: the href as written (http, https, mailto, file, or a plain path). `path`: the path as written, with any line suffix. `chat`: the chat link as written. */
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

const CHAT_LINK = /^helm:\/\/chats\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:\/messages\/(\d{1,15}))?$/;

/**
 * A chat link: `helm://chats/<id>`, optionally `/messages/<number>`. Strict on purpose: lowercase
 * canonical id, no user info, port, query, fragment, trailing slash or extra segments.
 */
export function parseChatLink(href: string): { chatId: string; messageId?: number } | null {
  const m = CHAT_LINK.exec(href);
  if (!m) return null;
  return m[2] === undefined ? { chatId: m[1] as string } : { chatId: m[1] as string, messageId: Number(m[2]) };
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
    if (name === 'helm') return parseChatLink(href) ? { kind: 'chat', href } : null;
    return null;
  }

  // A relative or drive path. Markdown notes are vault targets; anything else is a file path
  // the handler may decide to open.
  const hash = href.indexOf('#');
  const pathPart = safeDecode(hash < 0 ? href : href.slice(0, hash));
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
  if (kind !== 'url' && kind !== 'vault' && kind !== 'path' && kind !== 'chat') return null;
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
