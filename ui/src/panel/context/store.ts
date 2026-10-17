// What the side panel is showing next to each chat, as a context item, plus the
// pending review comments per chat. Bodies report their selection here; the composer's send
// contributor and the "Looking at: …" chip read it. Notifies through the panel store.

import type { ContextItem, ReviewComment } from '../../api/panel.ts';
import { entrySlot } from '../model.ts';
import { panelStore } from '../registry.ts';

/** The per-chat eye toggle, as last read from or written to the host. */
export const contextOn = new Map<string, boolean>();
/** A body's own item, keyed by tab id or `changes:<chat>`. */
const reported = new Map<string, ContextItem | null>();
/** Pending review comments per chat. */
export const pendingComments = new Map<string, ReviewComment[]>();
/** The review comment the user last clicked, per chat (it becomes the context item). */
const focusedComment = new Map<string, string>();

export function report(key: string, item: ContextItem | null): void {
  const before = JSON.stringify(reported.get(key) ?? null);
  if (before === JSON.stringify(item)) return;
  reported.set(key, item);
  panelStore.changed();
}

export function setComments(chatId: string, list: ReviewComment[]): void {
  pendingComments.set(chatId, list);
  if (!list.some((c) => c.id === focusedComment.get(chatId))) focusedComment.delete(chatId);
  panelStore.changed();
}

export function focusComment(chatId: string, id: string | null): void {
  if (id) focusedComment.set(chatId, id);
  else focusedComment.delete(chatId);
  panelStore.changed();
}

/** The item open next to `chatId`: the shown tab or the Changes slot's selection or comment. */
export function currentItem(chatId: string): ContextItem | null {
  const m = panelStore.model;
  if (!m.visible(chatId)) return null;
  const entry = m.activeEntry(chatId);
  if (!entry) return null;
  if (entrySlot(entry) === 'changes') {
    const cid = focusedComment.get(chatId);
    const c = cid ? pendingComments.get(chatId)?.find((x) => x.id === cid) : undefined;
    if (c) {
      return { kind: 'reviewComment', id: c.id, path: c.path, side: c.side, startLine: c.startLine, endLine: c.endLine, quote: c.quote, body: c.body };
    }
    return reported.get(`changes:${chatId}`) ?? null;
  }
  if (entrySlot(entry)) return null;
  return reported.get(entry) ?? null;
}

/** A short label for the chip: the file name and the selected lines. */
export function itemLabel(item: ContextItem): string {
  const base = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
  switch (item.kind) {
    case 'file':
      return item.selection ? `${base(item.path)}, lines ${item.selection.startLine}–${item.selection.endLine}` : base(item.path);
    case 'folder':
      return `${base(item.path)} folder`;
    case 'diff':
      return `${base(item.path)} diff, lines ${item.startLine}–${item.endLine}`;
    case 'reviewComment':
      return `comment on ${base(item.path)}`;
  }
}
