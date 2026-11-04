// Saved for later: the item shapes, the op wrappers, and the plain logic the Settings pane and the
// transcript's "Save for later" actions share — how text becomes a title, the reminder presets and
// the one-line "Due in …" label.

import type { Api } from '../api/client.ts';

export type SavedState = 'saved' | 'filed' | 'done' | 'dismissed';

export interface SavedItem {
  id: string;
  title: string;
  body: string;
  state: SavedState;
  chatId: string | null;
  messageId: number | null;
  captureKey: string | null;
  createdAt: number;
  updatedAt: number;
  revision: number;
  remindAt: number | null;
  remindedAt: number | null;
  noteCount: number;
}

export interface SavedNote {
  id: number;
  itemId: string;
  authorType: 'user' | 'agent';
  authorId: string | null;
  text: string;
  createdAt: number;
}

export interface SavedPage {
  items: SavedItem[];
  total: number;
  page: number;
  pageSize: number;
}

export const PAGE_SIZE = 50;

export const STATE_TITLES: Record<SavedState, string> = {
  saved: 'Saved',
  filed: 'Filed',
  done: 'Done',
  dismissed: 'Dismissed',
};

export function savedOps(api: Api) {
  return {
    save: (a: { title: string; body: string; chatId?: string; messageId?: number; captureKey?: string; remindAt?: number }) =>
      api<{ item: SavedItem; already: boolean }>('saved_save', a),
    list: (a: { state?: SavedState; query?: string; page?: number } = {}) => api<SavedPage>('saved_list', a),
    update: (a: { id: string; title?: string; body?: string; state?: SavedState; remindAt?: number | null; expectRevision?: number; remove?: boolean }) =>
      api<{ item?: SavedItem; removed?: boolean }>('saved_update', a),
    note: (a: { id: string; text: string }) => api<{ note: SavedNote }>('saved_note', a),
    comments: (a: { id: string }) => api<{ notes: SavedNote[] }>('saved_comments', a),
    /** The open items whose reminder is due now, whether or not this window was told as it came due. */
    due: () => api<{ items: SavedItem[] }>('saved_due', {}),
  };
}

/** The one line that says what is due: the item's title for one, a count for several. */
export function dueNotice(titles: string[]): string | null {
  if (titles.length === 0) return null;
  if (titles.length === 1) return `Saved for later, now due: ${titles[0]}`;
  return `${titles.length} saved items are due`;
}

/** Turn a message or selection into a saved item: the first line is the title, all of it is kept. */
export function savedFromText(text: string): { title: string; body: string } {
  const body = text.trim();
  const first = body.split('\n', 1)[0]?.trim() ?? '';
  const title = first.length > 120 ? `${first.slice(0, 119)}…` : first;
  return { title: title || 'Chat message', body };
}

export type RemindPreset = 'hour' | 'tonight' | 'tomorrow';

export const REMIND_PRESETS: { id: RemindPreset; label: string }[] = [
  { id: 'hour', label: 'In 1 hour' },
  { id: 'tonight', label: 'Tonight 9 pm' },
  { id: 'tomorrow', label: 'Tomorrow 9 am' },
];

/** The wall-clock time a preset names, from `now` (tonight rolls over once it is past 9 pm). */
export function remindAtFor(preset: RemindPreset, now: number): number {
  if (preset === 'hour') return now + 3_600_000;
  const d = new Date(now);
  d.setHours(preset === 'tonight' ? 21 : 9, 0, 0, 0);
  if (preset === 'tomorrow' || d.getTime() <= now) d.setDate(d.getDate() + 1);
  return d.getTime();
}

/** One quiet line about a pending reminder; null when there is nothing left to come. */
export function dueLabel(item: Pick<SavedItem, 'remindAt' | 'remindedAt'>, now: number): string | null {
  const { remindAt, remindedAt } = item;
  if (remindAt === null) return null;
  // The host clears reminded_at whenever it sets a new remind_at,
  // so a set reminded_at always means this reminder has already sounded.
  if (remindedAt !== null) return null;
  if (remindAt <= now) return 'Due now';
  const mins = Math.ceil((remindAt - now) / 60_000);
  if (mins < 60) return `Due in ${mins} min`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `Due in ${hours} h`;
  return `Due in ${Math.round(hours / 24)} d`;
}

/** The text a "Start a task" send puts in the new chat's draft. */
export function draftFromItem(item: SavedItem): string {
  return `Picking this back up:\n${item.title}\n\n${item.body}`;
}
