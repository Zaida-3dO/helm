// YAML frontmatter, as Obsidian shows it: a properties block, never raw text.
// A small subset is parsed (scalars, `[a, b]` lists, `- item` lists, block scalars); anything
// else becomes a value-only row rather than being dropped.

export interface Property {
  key: string;
  /** One entry for a scalar, several for a list. */
  values: string[];
  list: boolean;
}

export interface FrontmatterSplit {
  /** Null when the text has no frontmatter. */
  properties: Property[] | null;
  body: string;
}

function unquote(value: string): string {
  const v = value.trim();
  if (v.length >= 2 && ((v[0] === '"' && v.endsWith('"')) || (v[0] === "'" && v.endsWith("'")))) {
    return v.slice(1, -1);
  }
  return v;
}

function splitInlineList(inner: string): string[] {
  const out: string[] = [];
  let current = '';
  let quote = '';
  for (const ch of inner) {
    if (quote) {
      if (ch === quote) quote = '';
      current += ch;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      current += ch;
    } else if (ch === ',') {
      out.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  out.push(current);
  return out.map(unquote).filter((s) => s !== '');
}

const KEY_LINE = /^([^\s:#-][^:]*?):(?:[ \t]+(.*))?$/;
const LIST_LINE = /^[ \t]*-(?:[ \t]+(.*))?$/;

function parseProperties(lines: string[]): Property[] {
  const props: Property[] = [];
  let last: Property | null = null;
  let block: string[] | null = null;

  const flushBlock = () => {
    if (block && last) last.values = [block.join('\n').trim()];
    block = null;
  };

  for (const line of lines) {
    if (block) {
      if (line.trim() === '' || /^[ \t]/.test(line)) {
        block.push(line.trim());
        continue;
      }
      flushBlock();
    }
    if (line.trim() === '' || line.trim().startsWith('#')) continue;

    const key = KEY_LINE.exec(line);
    if (key) {
      const rawValue = (key[2] ?? '').trim();
      const prop: Property = { key: (key[1] as string).trim(), values: [], list: false };
      props.push(prop);
      last = prop;
      if (rawValue === '|' || rawValue === '>' || rawValue === '|-' || rawValue === '>-') {
        block = [];
      } else if (rawValue.startsWith('[') && rawValue.endsWith(']')) {
        prop.list = true;
        prop.values = splitInlineList(rawValue.slice(1, -1));
      } else if (rawValue !== '') {
        prop.values = [unquote(rawValue)];
      }
      continue;
    }

    const item = LIST_LINE.exec(line);
    if (item && last) {
      last.list = true;
      const value = unquote(item[1] ?? '');
      if (value) last.values.push(value);
      continue;
    }

    if (/^[ \t]/.test(line) && last && !last.list && last.values.length > 0) {
      // A continuation line of a folded scalar.
      last.values[0] = `${last.values[0]} ${line.trim()}`;
      continue;
    }

    // Not understood: keep it visible as a value-only row.
    props.push({ key: '', values: [line.trim()], list: false });
    last = null;
  }
  flushBlock();
  return props;
}

const OPENING = /^﻿?---[ \t]*\n/;
const CLOSING = /^(?:---|\.\.\.)[ \t]*$/;
const PLAUSIBLE_LINE = /^(?:[ \t]*$|[ \t]*#|[ \t]+\S|[ \t]*-(?:[ \t]|$)|[^\s:#][^:]*:(?:[ \t]|$))/;

/**
 * Splits leading frontmatter off `text` (newlines already normalised to \n).
 *
 * Unclosed frontmatter is ordinary Markdown, except while `streaming` and the lines so far all
 * look like properties: then the block is shown as it grows instead of flickering as a rule and
 * a heading.
 */
export function splitFrontmatter(text: string, streaming = false): FrontmatterSplit {
  const open = OPENING.exec(text);
  if (!open) return { properties: null, body: text };
  const lines = text.slice(open[0].length).split('\n');
  const closeAt = lines.findIndex((l) => CLOSING.test(l));
  if (closeAt >= 0) {
    return {
      properties: parseProperties(lines.slice(0, closeAt)),
      body: lines.slice(closeAt + 1).join('\n'),
    };
  }
  if (streaming && lines.every((l) => PLAUSIBLE_LINE.test(l))) {
    return { properties: parseProperties(lines), body: '' };
  }
  return { properties: null, body: text };
}
