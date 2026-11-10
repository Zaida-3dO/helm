// @-mentions in the PR view's text boxes: who can be suggested, and the text edits that go with it.
// Only people already on the pull request are suggested; Helm does not search GitHub's users.

import type { PrSummary, Thread, TimelineItem } from './types.ts';

function threadAuthors(t: Thread): string[] {
  return t.comments.map((c) => c.author);
}

/** The people on the pull request, in order of first appearance, without duplicates. */
export function participantLogins(summary: PrSummary | null, items: readonly TimelineItem[] | null): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  const add = (login: string | null | undefined) => {
    if (!login || seen.has(login.toLowerCase())) return;
    seen.add(login.toLowerCase());
    out.push(login);
  };
  if (summary) [summary.author, ...summary.reviewers, ...summary.assignees].forEach(add);
  for (const i of items ?? []) {
    if (i.type === 'comment' || i.type === 'review') add(i.author);
    if (i.type === 'review') i.threads.flatMap(threadAuthors).forEach(add);
    if (i.type === 'thread') threadAuthors(i.thread).forEach(add);
  }
  return out;
}

/** The `@name` being typed just before the caret, or null. `start` is the index of the `@`. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  const m = /(?:^|[\s(])@([A-Za-z0-9-]*)$/.exec(text.slice(0, caret));
  if (!m) return null;
  const query = m[1] ?? '';
  return { start: caret - query.length - 1, query };
}

/** Up to `max` logins that start with the query (then ones that contain it), ignoring case. */
export function matchLogins(all: readonly string[], query: string, max = 6): string[] {
  const q = query.toLowerCase();
  const starts = all.filter((l) => l.toLowerCase().startsWith(q));
  const inside = all.filter((l) => !l.toLowerCase().startsWith(q) && l.toLowerCase().includes(q));
  return [...starts, ...inside].slice(0, max);
}

/** `text` with the `@query` that begins at `start` replaced by `@login ` and the caret after it. */
export function insertMention(text: string, start: number, caret: number, login: string): { text: string; caret: number } {
  const next = `${text.slice(0, start)}@${login} ${text.slice(caret)}`;
  return { text: next, caret: start + login.length + 2 };
}
