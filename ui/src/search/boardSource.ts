// The Board group in Ctrl+K: items on the task board, read through the board client's attach-only
// path, so typing can never be what first connects to the board. The group exists only once a
// board connection is saved. The typed text reaches the board server (two or more characters),
// 300 ms after the last keystroke, with an 8 s deadline, 10 rows at most. When the whole text is
// an item id prefix the item is looked up too, and its row comes first. Answers are cached for the
// life of the dialog, in memory only. Rows are plain text; choosing one opens Tasks at that item.

import { itemIdPrefix, normaliseBoardQuery, shortId } from '../tasks/model.ts';
import { toBoardError, type BoardReader } from '../tasks/reader.ts';
import { rowId, type Row, type SearchSource } from './sources.ts';

/** The framework already waits 120 ms; this adds the rest of the 300 ms. */
export const BOARD_EXTRA_WAIT_MS = 180;
export const BOARD_ROWS = 10;
export const BOARD_DEADLINE_MS = 8_000;

export interface BoardDeps {
  reader: BoardReader;
  /** Whether a board connection is saved (looks it up when not yet known). */
  available: () => Promise<boolean>;
  /** Open the Tasks tab, at an item's drawer when given. */
  openTasks: (itemId?: string) => void;
  /** Close the search dialog. */
  close: () => void;
  wait?: (ms: number, signal: AbortSignal) => Promise<void>;
}

const waitFor = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    if (signal.aborted) return resolve();
    const t = setTimeout(resolve, ms);
    signal.addEventListener('abort', () => {
      clearTimeout(t);
      resolve();
    }, { once: true });
  });

export function boardSource(d: BoardDeps): SearchSource {
  const cache = new Map<string, Row[]>();
  /** Set when the board said it could not be asked (not started, or a failure): not asked again here. */
  let stopped: Row[] | Error | null = null;
  const wait = d.wait ?? waitFor;

  const row = (id: string, title: string, state: string, headline: string | null): Row => ({
    id: rowId('board', id),
    title,
    subtitle: `${state} · ${shortId(id)}`,
    snippet: headline?.trim() || undefined,
    open: () => {
      d.openTasks(id);
      d.close();
    },
  });

  return {
    id: 'board',
    minChars: 2,
    timeoutMs: BOARD_DEADLINE_MS,
    run: async (query, signal) => {
      const text = normaliseBoardQuery(query);
      if (text === null) return [];
      if (!(await d.available())) return [];
      if (stopped instanceof Error) throw stopped;
      if (stopped) return stopped;
      const hit = cache.get(text);
      if (hit) return hit;
      await wait(BOARD_EXTRA_WAIT_MS, signal);
      if (signal.aborted) return [];
      const opts = { attachOnly: true, timeoutMs: BOARD_DEADLINE_MS, signal };
      const prefix = itemIdPrefix(text);
      const [found, byId] = await Promise.allSettled([
        d.reader.read({ action: 'search', query: text, limit: BOARD_ROWS }, opts),
        prefix ? d.reader.read({ action: 'item', id: prefix }, opts) : Promise.resolve(null),
      ]);
      if (signal.aborted) return [];
      if (found.status === 'rejected') {
        const e = toBoardError(found.reason);
        if (e.code === 'cancelled') return [];
        if (e.code === 'not_started' || e.code === 'not_configured') {
          stopped = [
            {
              id: rowId('board', 'open-tasks'),
              title: 'Open Tasks to search the board',
              open: () => {
                d.openTasks();
                d.close();
              },
            },
          ];
          return stopped;
        }
        if (e.code === 'busy') throw new Error(e.message);
        stopped = new Error(e.code === 'timed_out' ? "the task board didn't answer in time." : e.message || 'the task board search failed.');
        throw stopped;
      }
      const rows: Row[] = [];
      const seen = new Set<string>();
      const add = (r: Row, id: string) => {
        if (!seen.has(id) && rows.length < BOARD_ROWS) {
          seen.add(id);
          rows.push(r);
        }
      };
      // The id lookup goes first; if it failed there is simply no id row.
      if (byId.status === 'fulfilled' && byId.value && 'item' in byId.value) {
        const i = byId.value.item;
        add(row(i.id, i.title, i.state, i.headline), i.id);
      }
      for (const m of found.value.matches ?? []) add(row(m.id, m.title, m.state, m.headline), m.id);
      cache.set(text, rows);
      return rows;
    },
  };
}
