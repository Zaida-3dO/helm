// Pure rules of the rename suggestions (Rename chat in the sidebar and the top bar), kept out of the
// .tsx so node:test can import them.

export const MAX_OPTIONS = 3;

/** The options to offer from the host's `title_suggest` answer: its `titles` (or the single `title`
 * of an older host), trimmed, one line each, none repeated (ignoring case) or equal to what the
 * field held when the box opened, at most three. Anything else gives none. */
export function suggestionOptions(answer: unknown, opened: string): string[] {
  const a = (answer ?? {}) as { titles?: unknown; title?: unknown };
  const raw = Array.isArray(a.titles) ? a.titles : typeof a.title === 'string' ? [a.title] : [];
  const out: string[] = [];
  const seen = new Set([opened.trim().toLowerCase()]);
  for (const t of raw) {
    if (typeof t !== 'string') continue;
    const s = t.trim();
    if (!s || /[\u0000-\u001f\u007f]/.test(s) || seen.has(s.toLowerCase())) continue;
    seen.add(s.toLowerCase());
    out.push(s);
    if (out.length >= MAX_OPTIONS) break;
  }
  return out;
}

/** Suggestions are offered only while the field still holds what it held when the box opened: once
 * the person has typed, their text is theirs and a suggestion never replaces it. */
export function suggestionsVisible(value: string, opened: string): boolean {
  return value === opened;
}
