// Obsidian callouts: `> [!type]+ Title`. Aliases fold onto the standard types so CSS needs one rule each.

const ALIASES: Record<string, string> = {
  note: 'note',
  abstract: 'abstract',
  summary: 'abstract',
  tldr: 'abstract',
  info: 'info',
  todo: 'todo',
  tip: 'tip',
  hint: 'tip',
  important: 'tip',
  success: 'success',
  check: 'success',
  done: 'success',
  question: 'question',
  help: 'question',
  faq: 'question',
  warning: 'warning',
  caution: 'warning',
  attention: 'warning',
  failure: 'failure',
  fail: 'failure',
  missing: 'failure',
  danger: 'danger',
  error: 'danger',
  bug: 'bug',
  example: 'example',
  quote: 'quote',
  cite: 'quote',
};

/** The standard type a callout type renders as; unknown types look like a note (as in Obsidian). */
export function canonicalCalloutType(type: string): string {
  return ALIASES[type.toLowerCase()] ?? 'note';
}

/** The title shown when the callout gives none: the type, capitalised. */
export function defaultCalloutTitle(type: string): string {
  const t = type.replace(/[-_]+/g, ' ').trim();
  return t.charAt(0).toUpperCase() + t.slice(1).toLowerCase();
}

export interface CalloutHead {
  type: string;
  /** '' not foldable, '+' foldable and open, '-' foldable and closed. */
  fold: '' | '+' | '-';
  title: string;
  /** The rest of the first paragraph after the marker line. */
  rest: string;
}

const HEAD = /^\[!([A-Za-z0-9_-]+)\]([+-]?)[ \t]*([^\n]*)(?:\n([\s\S]*))?$/;

/** Reads the first paragraph of a blockquote. Null when it is an ordinary quote. */
export function parseCalloutHead(paragraphText: string): CalloutHead | null {
  const m = HEAD.exec(paragraphText);
  if (!m) return null;
  return {
    type: m[1] as string,
    fold: (m[2] ?? '') as '' | '+' | '-',
    title: (m[3] ?? '').trim(),
    rest: m[4] ?? '',
  };
}
