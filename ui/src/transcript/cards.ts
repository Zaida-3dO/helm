// The cards an agent puts in the chat by calling a host tool: a gallery of pictures
// (app_show_images) and an option board (app_offer_options). The tool's result is JSON text that
// carries the id of what the host stored; the card reads everything else from the host by that id.
// Nothing here guesses from a reply's prose: a card exists only because a tool was called.
//
// Two more tools ask the PC showing the chat to do something: show a file or a page in the side
// panel (app_open_panel) and forward a port of the PC the chat runs on (app_forward_port). Their
// rows are read here too, and `autoOpenPicks` decides which of them act by themselves.

import type { MessageRow } from '../api/types.ts';
import type { OptionBoard } from '../api/ops.ts';
import { helmFileHref, parseHelmLink, type LinkTarget } from '../markdown/links.ts';
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

/** A file or page an app_open_panel call showed, as a link that goes through the link gate. */
export interface PanelRef {
  kind: 'file' | 'page';
  target: LinkTarget;
  /** What the card's button names: the file's name or the page's title. */
  label: string;
}

/** A port an app_forward_port call offered from the PC the chat runs on. */
export interface ForwardRef {
  port: number;
  title: string;
  /** The person typed the last message on the PC the chat runs on, so localhost already works there. */
  samePc: boolean;
  /** The PC the last message was typed on, when known. */
  typedOn: string | null;
}

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/g;

function cleanText(v: unknown, max: number): string {
  return typeof v === 'string' ? v.replace(CONTROL, ' ').trim().slice(0, max) : '';
}

function finishedCall(row: MessageRow, tool: string): Record<string, unknown> | null {
  if (hostTool(row) !== tool || row.status !== 'complete' || row.details?.isError) return null;
  return resultObject(row);
}

/**
 * The file or page a finished app_open_panel call showed. Built into a `helm://` link and checked by
 * the same strict parser the link gate uses, so a result naming a share, a device path or a bad pin
 * id is no card at all.
 */
export function panelRef(row: MessageRow): PanelRef | null {
  const r = finishedCall(row, 'app_open_panel');
  const o = r?.opened;
  if (!o || typeof o !== 'object' || Array.isArray(o)) return null;
  const opened = o as Record<string, unknown>;
  if (opened.kind === 'file' && typeof opened.path === 'string') {
    const line = typeof opened.line === 'number' && Number.isInteger(opened.line) && opened.line >= 1 ? opened.line : undefined;
    const href = helmFileHref(opened.path, line);
    if (parseHelmLink(href)?.form !== 'file') return null;
    const name = opened.path.split(/[\\/]/).filter(Boolean).pop() ?? opened.path;
    return { kind: 'file', target: { kind: 'helm', href }, label: cleanText(name, 120) || 'file' };
  }
  if (opened.kind === 'page' && typeof opened.pageId === 'string') {
    const href = `helm://pages/${opened.pageId}`;
    if (parseHelmLink(href)?.form !== 'page') return null;
    return { kind: 'page', target: { kind: 'helm', href }, label: cleanText(opened.title, 120) || 'page' };
  }
  return null;
}

/** The port a finished app_forward_port call offered. */
export function forwardRef(row: MessageRow): ForwardRef | null {
  const r = finishedCall(row, 'app_forward_port');
  const f = r?.forward;
  if (!f || typeof f !== 'object' || Array.isArray(f)) return null;
  const port = (f as Record<string, unknown>).port;
  if (typeof port !== 'number' || !Number.isInteger(port) || port < 1 || port > 65535) return null;
  const typedOn = cleanText(r!.typedOn, 40);
  return { port, title: cleanText((f as Record<string, unknown>).title, 200), samePc: r!.samePc === true, typedOn: typedOn || null };
}

export interface AutoOpenPicks {
  panel: { rowId: number; ref: PanelRef } | null;
  forward: { rowId: number; ref: ForwardRef } | null;
}

/**
 * Which rows of one live batch act by themselves: at most one panel open and one forward, the
 * batch's last of each. A row acts only when it is newer than every row the chat had when it was
 * last read in full (`after`: history and a reconnect's catch-up never act), and only once: every
 * row looked at here is added to `done`, acted on or not. A forward acts only for a chat that runs
 * on another PC, and not when the person is at that PC already.
 */
export function autoOpenPicks(rows: readonly MessageRow[], opts: { after: number; done: Set<number>; remote: boolean }): AutoOpenPicks {
  const out: AutoOpenPicks = { panel: null, forward: null };
  for (const row of rows) {
    if (row.role !== 'toolResult' || row.id <= opts.after || opts.done.has(row.id)) continue;
    const panel = panelRef(row);
    const forward = panel ? null : forwardRef(row);
    if (!panel && !forward) continue;
    opts.done.add(row.id);
    if (panel) out.panel = { rowId: row.id, ref: panel };
    else if (forward && opts.remote && !forward.samePc) out.forward = { rowId: row.id, ref: forward };
  }
  return out;
}
