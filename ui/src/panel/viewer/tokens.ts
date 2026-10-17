// Highlighter tokens as character ranges, so the code view can draw highlight.js' colours as
// CodeMirror marks over the plain text.

import { highlightCode } from '../../markdown/highlight.ts';

export interface TokenRange {
  from: number;
  to: number;
  className: string;
}

const ENTITIES: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'", '#x27': "'" };

/**
 * Reads highlight.js output (escaped text and nested `<span class="…">`) back into ranges over
 * the source text. Returns null if the markup does not match the source, so callers fall back to
 * plain text rather than drawing misplaced colours.
 */
export function rangesFromHtml(html: string, source: string): TokenRange[] | null {
  const out: TokenRange[] = [];
  const stack: Array<{ from: number; className: string }> = [];
  let pos = 0;
  let i = 0;
  while (i < html.length) {
    const ch = html[i] as string;
    if (ch === '<') {
      const end = html.indexOf('>', i);
      if (end < 0) return null;
      const tag = html.slice(i + 1, end);
      if (tag === '/span') {
        const open = stack.pop();
        if (!open) return null;
        if (pos > open.from) out.push({ from: open.from, to: pos, className: open.className });
      } else {
        const m = /^span class="([^"]*)"$/.exec(tag);
        if (!m) return null;
        stack.push({ from: pos, className: m[1] as string });
      }
      i = end + 1;
      continue;
    }
    let text = ch;
    if (ch === '&') {
      const end = html.indexOf(';', i);
      const name = end < 0 ? '' : html.slice(i + 1, end);
      const decoded = ENTITIES[name];
      if (decoded === undefined) return null;
      text = decoded;
      i = end + 1;
    } else {
      i++;
    }
    if (source[pos] !== text) return null;
    pos++;
  }
  if (stack.length > 0 || pos !== source.length) return null;
  return out;
}

/** Token ranges for `source` in `language`; empty when the language is unknown or too large. */
export function tokenRanges(source: string, language: string | undefined): TokenRange[] {
  if (!language) return [];
  const html = highlightCode(source, language);
  if (!html.includes('<span')) return [];
  return rangesFromHtml(html, source) ?? [];
}
