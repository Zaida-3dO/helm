// "Load more chats". One click reads pages of 500 until it
// reaches a page whose first chat the window does not hold yet, the list ends, 10 pages that
// brought new chats were read, or the read budget is spent: 50 reads plus one per 500 chats held
// (counted once per click, rounded up), so a click always gets past what it already holds even
// past about 25,000 chats. Every page with new chats is merged as soon as it arrives, so rows
// gained from an earlier page survive a later failure.

export const PAGE_SIZE = 500;
export const PRODUCTIVE_PAGES = 10;
export const BASE_READS = 50;

export interface PageResult<R> {
  rows: R[];
  nextCursor: string | null;
}

export interface LoadMoreResult {
  cursor: string | null;
  reads: number;
  productive: number;
  /** The list ended (no next cursor): the view is complete. */
  complete: boolean;
}

export function readBudget(held: number): number {
  return BASE_READS + Math.ceil(held / PAGE_SIZE);
}

export async function loadMore<R>(opts: {
  cursor: string | null;
  held: (row: R) => boolean;
  heldCount: number;
  fetch: (cursor: string | null) => Promise<PageResult<R>>;
  merge: (rows: R[]) => void;
}): Promise<LoadMoreResult> {
  const budget = readBudget(opts.heldCount);
  let cursor = opts.cursor;
  let reads = 0;
  let productive = 0;
  for (;;) {
    const page = await opts.fetch(cursor);
    reads += 1;
    const fresh = page.rows.filter((r) => !opts.held(r));
    if (fresh.length > 0) {
      opts.merge(fresh);
      productive += 1;
    }
    cursor = page.nextCursor;
    const first = page.rows[0];
    const reachedNew = first !== undefined && fresh.includes(first);
    if (cursor === null) return { cursor, reads, productive, complete: true };
    if (reachedNew || productive >= PRODUCTIVE_PAGES || reads >= budget) {
      return { cursor, reads, productive, complete: false };
    }
  }
}
