// What a paste into the composer turns into, before the editor inserts it.
//
// - A very long plain-text paste becomes an attached text file instead of filling the draft.
// - A rich-text paste (the clipboard also holds HTML) keeps its links, as Markdown `[label](url)`,
//   but only when the HTML shows exactly the same text as the plain version; anything else pastes
//   the plain text unchanged.
//
// Pure functions on strings: the HTML is read by a small tolerant scanner, never handed to the
// page, so nothing in a pasted page can run or load.

/** A paste of at least this many lines is attached as a file. */
export const LONG_PASTE_LINES = 250;
/** ... or of at least this many characters. */
export const LONG_PASTE_CHARS = 20_000;
/** The host keeps at most this many bytes of one saved file. */
const MAX_FILE_BYTES = 25 * 1024 * 1024;

const LIMITS = { plain: 256_000, html: 1024 * 1024, nodes: 10_000, links: 2_000, href: 8 * 1024 };

/** Whether this text is long enough to become a file. Text the host would refuse as too big stays inline. */
export function isLongPaste(text: string): boolean {
  if (text.length < LONG_PASTE_CHARS && lineCount(text) < LONG_PASTE_LINES) return false;
  return text.length * 3 <= MAX_FILE_BYTES || new TextEncoder().encode(text).length <= MAX_FILE_BYTES;
}

export function lineCount(text: string): number {
  if (!text) return 0;
  const n = text.split('\n').length;
  return text.endsWith('\n') ? n - 1 : n;
}

/** The file's extension, guessed from what was pasted: a unified diff, JSON, or plain text. */
export function guessExtension(text: string): 'diff' | 'json' | 'txt' {
  const head = text.trimStart();
  const hunks = /^@@ -\d+(,\d+)? \+\d+(,\d+)? @@/m.test(head) && /^[-+]/m.test(head);
  if (/^(diff --git |--- .+\n\+\+\+ |Index: )/.test(head) || hunks) return 'diff';
  if (head.startsWith('{') || head.startsWith('[')) {
    try {
      JSON.parse(text);
      return 'json';
    } catch {
      // not JSON after all
    }
  }
  return 'txt';
}

/** `pasted-312-lines.json`: named after what it holds. */
export function pastedFileName(text: string): string {
  const n = lineCount(text);
  return `pasted-${n}-${n === 1 ? 'line' : 'lines'}.${guessExtension(text)}`;
}

// ---- rich-text paste --------------------------------------------------------------------------

/** Tags whose content is never shown; scanned past entirely. */
const HIDDEN = new Set(['script', 'style', 'template', 'noscript', 'iframe', 'object', 'embed', 'svg', 'math', 'head', 'title']);
const VOID = new Set(['br', 'hr', 'img', 'input', 'meta', 'link', 'wbr', 'col', 'area', 'base', 'source', 'track']);

const NAMED: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };

function decodeEntities(s: string): string {
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, e: string) => {
    if (e[0] === '#') {
      const code = e[1] === 'x' || e[1] === 'X' ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : m;
    }
    return NAMED[e.toLowerCase()] ?? m;
  });
}

interface Link {
  href: string;
  /** The label's span in the whitespace-free visible text. */
  from: number;
  to: number;
}

/** The visible text of an HTML fragment with its whitespace dropped, and where its links sit. */
function scan(html: string): { text: string; links: Link[] } | null {
  const links: Link[] = [];
  let text = '';
  let nodes = 0;
  let hidden: string | null = null;
  let hiddenDepth = 0;
  let open: { href: string; from: number; depth: number } | null = null;
  let depth = 0;
  const re = /<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<(\/?)([a-zA-Z][a-zA-Z0-9:-]*)((?:"[^"]*"|'[^']*'|[^'">])*)>|<![^>]*>|<\?[^>]*>|([^<]+|<)/g;
  for (let m = re.exec(html); m; m = re.exec(html)) {
    if (m[2] === undefined) {
      if (m[4] !== undefined && hidden === null) text += decodeEntities(m[4]).replace(/\s+/g, '');
      continue;
    }
    if (++nodes > LIMITS.nodes) return null;
    const closing = m[1] === '/';
    const tag = m[2].toLowerCase();
    const selfClosed = m[3]!.trimEnd().endsWith('/');
    if (hidden !== null) {
      if (tag === hidden && !selfClosed) {
        hiddenDepth += closing ? -1 : 1;
        if (hiddenDepth === 0) hidden = null;
      }
      continue;
    }
    if (!closing && HIDDEN.has(tag)) {
      if (!selfClosed) {
        hidden = tag;
        hiddenDepth = 1;
      }
      continue;
    }
    if (VOID.has(tag) || selfClosed) continue;
    if (tag === 'a') {
      if (!closing) {
        // A link inside a link is not a link of its own: the outer one wins.
        if (open === null) {
          const href = /\shref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))/i.exec(m[3]!);
          open = { href: decodeEntities(href?.[1] ?? href?.[2] ?? href?.[3] ?? ''), from: text.length, depth };
        }
        depth++;
      } else {
        depth = Math.max(0, depth - 1);
        if (open && open.depth === depth) {
          if (text.length > open.from) links.push({ href: open.href, from: open.from, to: text.length });
          open = null;
        }
      }
    }
  }
  return { text, links };
}

/** An external web address worth keeping as a link, or null. */
function externalUrl(href: string): string | null {
  const h = href.trim();
  if (!h || h.length > LIMITS.href || /[\u0000-\u001f\u007f]/.test(h)) return null;
  return /^https?:\/\/[^\s/?#]+/i.test(h) ? h : null;
}

const escapeLabel = (s: string) => s.replace(/[\\`*_~[\]<>]/g, (c) => `\\${c}`);
const encodeUrl = (u: string) => u.replace(/[()<>\\\s]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0')}`);

/**
 * The plain text with each external link written as `[label](url)`, or null when nothing changes
 * (no HTML, a mismatch, over a limit, or no link worth converting): the caller then pastes the
 * plain text as it is.
 */
export function linkedPaste(plain: string, html: string): string | null {
  if (!plain || !html || plain.length > LIMITS.plain || html.length > LIMITS.html) return null;
  if (!/<a[\s>]/i.test(html)) return null;
  const seen = scan(html);
  if (!seen || seen.links.length === 0 || seen.links.length > LIMITS.links) return null;

  // Whitespace-free plain text, remembering where each of its characters sits in the real one.
  const at: number[] = [];
  let flat = '';
  for (let i = 0; i < plain.length; i++) {
    if (/\s/.test(plain[i]!)) continue;
    flat += plain[i];
    at.push(i);
  }
  if (flat !== seen.text) return null;

  let out = '';
  let done = 0;
  let changed = false;
  for (const link of seen.links) {
    const url = externalUrl(link.href);
    const start = at[link.from];
    const end = at[link.to - 1];
    if (!url || start === undefined || end === undefined || start < done) continue;
    const label = plain.slice(start, end + 1);
    // A label that is the address itself, or spans lines, stays as it was typed.
    if (label === url || label === link.href.trim() || label.includes('\n')) continue;
    out += plain.slice(done, start) + `[${escapeLabel(label)}](${encodeUrl(url)})`;
    done = end + 1;
    changed = true;
  }
  return changed ? out + plain.slice(done) : null;
}
