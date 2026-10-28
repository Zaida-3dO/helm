// The Tasks view's small pure rules: which links are safe to draw as links, how a card's holders
// are matched against the fleet snapshot, what counts as an item id, how typed search text is
// cleaned, how a column's count reads, and how pages merge. Plain TypeScript, tested under node:test.

import type { BoardEntry, BoardHolder, FleetAssignment } from '../api/board.ts';

/** A link drawn as a link only when it is http(s) and carries no credentials; otherwise inert text. */
export function safeHttpUrl(raw: string): string | null {
  try {
    const u = new URL(raw);
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
    if (u.username || u.password) return null;
    return u.href;
  } catch {
    return null;
  }
}

export type Holders =
  | { kind: 'pending' }
  | { kind: 'unavailable' }
  | { kind: 'none' }
  | { kind: 'lines'; lines: string[] };

const roleName = (h: BoardHolder) => (h.roleCustom && h.roleCustom.trim() ? h.roleCustom : h.role);
const holderKey = (h: BoardHolder) => `${h.holderId}|${h.role}|${h.roleCustom ?? ''}`;

/**
 * A card's holders. The card's own slim holders are matched one-for-one against the item's rows in
 * the complete fleet snapshot (which adds the machine). If the counts or the holders do not line
 * up, or the snapshot could not be read in full, the card says so rather than guess.
 */
export function holdersFor(entry: BoardEntry, fleet: { byItem: ReadonlyMap<string, FleetAssignment[]>; complete: boolean } | 'failed' | null): Holders {
  if (fleet === null) return { kind: 'pending' };
  if (fleet === 'failed' || !fleet.complete) return { kind: 'unavailable' };
  const live = [...(fleet.byItem.get(entry.item.id) ?? [])];
  const slim = entry.assignments ?? [];
  if (slim.length !== live.length) return { kind: 'unavailable' };
  if (slim.length === 0) return { kind: 'none' };
  const lines: string[] = [];
  for (const s of slim) {
    const at = live.findIndex((f) => holderKey(f) === holderKey(s));
    if (at < 0) return { kind: 'unavailable' };
    const [f] = live.splice(at, 1);
    lines.push(`${roleName(s)}: ${s.displayName} · ${f!.machine?.trim() ? f!.machine : 'machine unavailable'}`);
  }
  return { kind: 'lines', lines };
}

/** The fleet rows grouped by item id. */
export function groupFleet(rows: readonly FleetAssignment[]): Map<string, FleetAssignment[]> {
  const out = new Map<string, FleetAssignment[]>();
  for (const r of rows) {
    const list = out.get(r.itemId);
    if (list) list.push(r);
    else out.set(r.itemId, [r]);
  }
  return out;
}

/**
 * Search text as it is sent: every run of whitespace one space, control characters dropped, cut to
 * 200 characters and trimmed. Under two characters nothing is sent.
 */
export function normaliseBoardQuery(raw: string): string | null {
  // eslint-disable-next-line no-control-regex
  const flat = raw.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, '').replace(/\s+/g, ' ').trim();
  const cut = [...flat].slice(0, 200).join('').trim();
  return [...cut].length >= 2 ? cut : null;
}

/**
 * Whether the whole text is an item id prefix: 8 hex characters, or more of a canonical id in whole
 * hyphen-separated groups (8, 8-4, 8-4-4, 8-4-4-4 or the full 8-4-4-4-12). Sent lowercased.
 */
export function itemIdPrefix(text: string): string | null {
  const t = text.trim().toLowerCase();
  if (/^[0-9a-f]{8}$/.test(t)) return t;
  if (/^[0-9a-f]{8}(-[0-9a-f]{4}){1,3}$/.test(t)) return t;
  if (/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(t)) return t;
  return null;
}

/** The text of a board id on the screen: the first 8 characters. */
export const shortId = (id: string): string => id.slice(0, 8);

/** "n / total" when the board gave a total, "n loaded" when it did not, "…" before the first page. */
export function columnCount(loaded: number, total: number | null, started: boolean): string {
  if (!started) return '…';
  return total === null ? `${loaded} loaded` : `${loaded} / ${total}`;
}

/** Pages merged by item id, so a card never shows twice; a repeat keeps its first place. */
export function mergeEntries(old: readonly BoardEntry[], page: readonly BoardEntry[]): BoardEntry[] {
  const seen = new Set(old.map((e) => e.item.id));
  const out = [...old];
  for (const e of page) {
    if (seen.has(e.item.id)) continue;
    seen.add(e.item.id);
    out.push(e);
  }
  return out;
}

/** The sentence for a failed read, by its code. */
export function readError(code: string, message: string): string {
  switch (code) {
    case 'not_configured':
      return 'Add the board address and token in Settings, Projects to see tasks.';
    case 'token_rejected':
      return 'Agent Standup refused the token; check it in Settings, Projects.';
    case 'timed_out':
      return "The task board didn't answer in time.";
    case 'unreachable':
      return "Couldn't reach Agent Standup.";
    case 'busy':
      return message;
    default:
      return message || 'The task board could not be read.';
  }
}
