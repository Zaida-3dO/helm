// Find in chat: which loaded rows match a query, and in what order the
// reader steps through them. Matching is case-insensitive over each row's text; "Include activity"
// adds tool calls, thinking and context rows. Pure, so the bar and the tests share it.

import type { MessageRow } from '../api/types.ts';

export const FIND_MAX_QUERY = 256;
export const FIND_CAP = 10_000;

/** One match: the row it is in and which occurrence in that row (0-based, in text order). */
export interface FindMatch {
  rowId: number;
  occurrence: number;
}

/** The text a row is searched by, or '' when the row is not searched. */
export function searchableText(row: MessageRow, includeActivity: boolean): string {
  if (row.role === 'user') return row.text;
  if (row.role === 'assistant') return includeActivity && row.thinking ? `${row.thinking}\n${row.text}` : row.text;
  if (!includeActivity) return '';
  if (row.role === 'toolResult') {
    const args = typeof row.details?.toolArgs === 'string' ? row.details.toolArgs : '';
    return [row.toolName ?? '', args, row.text].join('\n');
  }
  return row.text;
}

function occurrences(haystack: string, needle: string, limit: number): number {
  let n = 0;
  for (let i = haystack.indexOf(needle); i >= 0 && n < limit; i = haystack.indexOf(needle, i + needle.length)) n++;
  return n;
}

/** Every match in the rows, oldest first, stopping at `cap`. `capped` says the list stopped early. */
export function findMatches(rows: readonly MessageRow[], query: string, includeActivity: boolean, cap = FIND_CAP): { matches: FindMatch[]; capped: boolean } {
  const q = query.slice(0, FIND_MAX_QUERY).toLowerCase();
  const matches: FindMatch[] = [];
  if (!q.trim()) return { matches, capped: false };
  for (const row of rows) {
    const text = searchableText(row, includeActivity).toLowerCase();
    if (!text) continue;
    const n = occurrences(text, q, cap - matches.length + 1);
    for (let k = 0; k < n; k++) {
      if (matches.length >= cap) return { matches, capped: true };
      matches.push({ rowId: row.id, occurrence: k });
    }
  }
  return { matches, capped: false };
}

/** Step through matches: "older" moves up the chat (Enter), "newer" down (Shift+Enter); both wrap. */
export function step(index: number, count: number, dir: 'older' | 'newer'): number {
  if (count === 0) return -1;
  if (index < 0) return count - 1;
  return dir === 'older' ? (index - 1 + count) % count : (index + 1) % count;
}

/** "3 / 120", with "+" when the list was capped; "No matches" for none. */
export function countLabel(index: number, count: number, capped: boolean): string {
  if (count === 0) return 'No matches';
  return `${index + 1} / ${count}${capped ? '+' : ''}`;
}
