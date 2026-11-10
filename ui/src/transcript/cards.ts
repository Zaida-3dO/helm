// The cards an agent puts in the chat by calling a host tool: a gallery of pictures
// (app_show_images) and an option board (app_offer_options). The tool's result is JSON text that
// carries the id of what the host stored; the card reads everything else from the host by that id.
// Nothing here guesses from a reply's prose: a card exists only because a tool was called.

import type { MessageRow } from '../api/types.ts';
import type { OptionBoard } from '../api/ops.ts';
import { toolIdentity } from './toolKinds.ts';

export type CardKind = 'gallery' | 'board';

function resultObject(row: MessageRow): Record<string, unknown> | null {
  const text = row.text.trim();
  if (!text.startsWith('{')) return null;
  try {
    const v: unknown = JSON.parse(text);
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function hostTool(row: MessageRow): string | null {
  if (row.role !== 'toolResult') return null;
  const id = toolIdentity(row);
  return id.source === 'host' ? id.tool : null;
}

/** The gallery a finished app_show_images call made: its id and how many pictures it holds. */
export function galleryRef(row: MessageRow): { galleryId: string; count: number } | null {
  if (hostTool(row) !== 'app_show_images' || row.status !== 'complete' || row.details?.isError) return null;
  const r = resultObject(row);
  if (!r || typeof r.galleryId !== 'string' || !r.galleryId) return null;
  return { galleryId: r.galleryId, count: typeof r.count === 'number' && r.count > 0 ? r.count : 0 };
}

/** The board a finished app_offer_options call made. */
export function boardRef(row: MessageRow): { boardId: string } | null {
  if (hostTool(row) !== 'app_offer_options' || row.status !== 'complete' || row.details?.isError) return null;
  const r = resultObject(row);
  return r && typeof r.boardId === 'string' && r.boardId ? { boardId: r.boardId } : null;
}

/** Which card a tool row is drawn as, or null for an ordinary tool row (a running or failed call stays one). */
export function cardKind(row: MessageRow): CardKind | null {
  if (galleryRef(row)) return 'gallery';
  if (boardRef(row)) return 'board';
  return null;
}

/** A card is the agent talking to the user, so it never folds away behind "worked for". */
export function isCardRow(row: MessageRow): boolean {
  return cardKind(row) !== null;
}

/**
 * The message a tap on the board sends, in the form the tool promised the agent: "<question>:
 * <choice>". A picked option's detail follows in brackets, so the agent hears what the tile said.
 */
export function optionMessage(board: Pick<OptionBoard, 'question' | 'options'>, labels: readonly string[], other?: string): string {
  const q = board.question.trim();
  if (other !== undefined) return `${q}: ${other.trim()}`;
  const picked = labels.map((l) => {
    const detail = board.options.find((o) => o.label === l)?.detail?.trim();
    return detail ? `${l} (${detail})` : l;
  });
  return `${q}: ${picked.join(', ')}`;
}

/** What the answered board says it chose. */
export function chosenText(answer: NonNullable<OptionBoard['answer']>): string {
  return answer.other ? `Other: ${answer.other}` : answer.labels.join(', ');
}

/** The key a number on the keyboard picks: 1 to 9, as an option index; null for anything else. */
export function optionForKey(key: string, count: number): number | null {
  if (!/^[1-9]$/.test(key)) return null;
  const i = Number(key) - 1;
  return i < count ? i : null;
}
