// Copy whole chat: select everything in the transcript (Ctrl+A) and copy (Ctrl+C). The list
// only mounts the rows near the viewport, so the text comes from the stored history, not the page.
// Pure parts, so they are tested.

import type { MessageRow, TranscriptPage } from '../api/types.ts';
import { bubbleText } from './origin.ts';

/** At most this many rows are gathered, so a huge chat cannot stall the copy. */
export const MAX_ROWS = 20_000;

/** The chat's conversation as Markdown: what the user wrote and what the agent answered. */
export function chatToMarkdown(rows: readonly MessageRow[]): string {
  const blocks: string[] = [];
  let last: 'user' | 'assistant' | null = null;
  for (const r of rows) {
    if (r.role === 'user') {
      const text = bubbleText(r).trim();
      if (!text) continue;
      blocks.push(`### You\n\n${text}`);
      last = 'user';
    } else if (r.role === 'assistant') {
      const text = r.text.trim();
      if (!text) continue;
      // Notes and the reply of one turn read as one answer.
      if (last === 'assistant') blocks.push(text);
      else blocks.push(`### Assistant\n\n${text}`);
      last = 'assistant';
    }
  }
  return blocks.join('\n\n') + (blocks.length ? '\n' : '');
}

/** Every row of the chat, oldest first, read a page at a time from the newest back. */
export async function loadWholeChat(
  page: (before?: string) => Promise<Pick<TranscriptPage, 'messages' | 'hasOlder' | 'olderCursor'>>,
): Promise<MessageRow[]> {
  let rows: MessageRow[] = [];
  let before: string | undefined;
  for (;;) {
    const p = await page(before);
    rows = [...p.messages, ...rows];
    if (!p.hasOlder || !p.olderCursor || rows.length >= MAX_ROWS) return rows.slice(-MAX_ROWS);
    before = p.olderCursor;
  }
}

/** A key that the page itself would handle as text editing: it is left alone. */
export function isEditable(target: EventTarget | null): boolean {
  const el = target instanceof Element ? target : null;
  if (!el) return false;
  return Boolean(el.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]'));
}
