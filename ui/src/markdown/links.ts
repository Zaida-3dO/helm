// Link targets: what a clicked link means to the app, and which hrefs are safe to emit at all.
//
// The renderer never navigates. Every anchor it produces carries its target in `data-md-*`
// attributes, and the component turns a click into an `onLink(target)` call (index.ts contract).

export interface LinkTarget {
  kind: 'url' | 'vault';
  /** `url` only: the href as written (http, https, mailto, file, or a plain path). */
  href?: string;
  /** `vault` only: the note name or path, without `.md`. Empty means the current note. */
  note?: string;
  heading?: string;
  block?: string;
  alias?: string;
}

const IMAGE_EXTENSIONS = /\.(png|jpe?g|gif|svg|webp|bmp|avif)$/i;

export function isImageName(name: string): boolean {
  return IMAGE_EXTENSIONS.test(name);
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
 * (javascript:, data:, vbscript:, protocol-relative, anything outside the allowlist).
 */
export function classifyHref(rawHref: string): LinkTarget | null {
  // Control characters inside the scheme are how `java\tscript:` gets past naive filters.
  // eslint-disable-next-line no-control-regex
  const href = rawHref.replace(/[\u0000-\u001f\u007f]/g, '').trim();
  if (!href) return null;
  if (href.startsWith('//')) return null;

  if (href.startsWith('#')) {
    return { kind: 'vault', ...splitNoteAndFragment(safeDecode(href)) };
  }

  const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(href);
  // A single letter is a Windows drive ("C:\notes\x.md"), not a scheme.
  if (scheme && (scheme[1] as string).length > 1) {
    const name = (scheme[1] as string).toLowerCase();
    if (name === 'http' || name === 'https' || name === 'mailto' || name === 'file') {
      return { kind: 'url', href };
    }
    if (name === 'obsidian') return classifyObsidianUri(href);
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
  if (!file) return null;
  return { kind: 'vault', ...splitNoteAndFragment(file.replace(/\.md$/i, '')) };
}

/** The `data-md-*` attributes that carry a target through the HTML string. */
export function targetAttrs(target: LinkTarget): Array<[string, string]> {
  const attrs: Array<[string, string]> = [['data-md-kind', target.kind]];
  if (target.href !== undefined) attrs.push(['data-md-href', target.href]);
  if (target.note !== undefined) attrs.push(['data-md-note', target.note]);
  if (target.heading) attrs.push(['data-md-heading', target.heading]);
  if (target.block) attrs.push(['data-md-block', target.block]);
  if (target.alias) attrs.push(['data-md-alias', target.alias]);
  return attrs;
}

/** Reads a target back from the attributes written by {@link targetAttrs}. */
export function targetFromAttrs(get: (name: string) => string | null): LinkTarget | null {
  const kind = get('data-md-kind');
  if (kind !== 'url' && kind !== 'vault') return null;
  const target: LinkTarget = { kind };
  const href = get('data-md-href');
  const note = get('data-md-note');
  const heading = get('data-md-heading');
  const block = get('data-md-block');
  const alias = get('data-md-alias');
  if (href !== null) target.href = href;
  if (note !== null) target.note = note;
  if (heading !== null) target.heading = heading;
  if (block !== null) target.block = block;
  if (alias !== null) target.alias = alias;
  return target;
}
