// The search dialog's logic without any DOM.
//
// A query settles for DEBOUNCE_MS, then every source runs at once. Each source settles on its own:
// rows when it answers, an error when it fails or passes its deadline. A source that fails, throws
// at once or never answers cannot hold up the others. A newer query aborts the older runs through
// their AbortSignal and drops anything they still deliver.

import { GROUP_LIMIT, groupOrder, type Row, type SearchSource, type SourceId } from './sources.ts';

export const DEBOUNCE_MS = 120;
export const MAX_WORDS = 12;
export const MAX_QUERY_BYTES = 512;
export const PAGE_STEP = 10;

export type GroupStatus = 'idle' | 'searching' | 'done' | 'error';

export interface GroupState {
  status: GroupStatus;
  rows: Row[];
  error: string | null;
}

export interface SearchState {
  /** The normalised query the groups belong to; empty when nothing is typed. */
  query: string;
  /** `searching` until every source that was asked has settled. */
  status: 'idle' | 'searching' | 'done';
  groups: Partial<Record<SourceId, GroupState>>;
}

export const IDLE: SearchState = { query: '', status: 'idle', groups: {} };

export interface Timers {
  set: (f: () => void, ms: number) => unknown;
  clear: (h: unknown) => void;
}

const realTimers: Timers = { set: (f, ms) => setTimeout(f, ms), clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>) };

/** The query as the host will read it: trimmed, words past the twelfth dropped, 512 bytes at most. */
export function normaliseQuery(raw: string): string {
  const words = raw.trim().split(/\s+/).filter(Boolean).slice(0, MAX_WORDS).join(' ');
  let cut = words;
  const enc = new TextEncoder();
  while (enc.encode(cut).length > MAX_QUERY_BYTES) cut = cut.slice(0, -1);
  return cut;
}

const settled = (rows: Row[]): GroupState => ({ status: 'done', rows, error: null });

export class UniversalSearch {
  private state: SearchState = IDLE;
  private readonly listeners = new Set<() => void>();
  private readonly sources: SearchSource[];
  private readonly timers: Timers;
  private timer: unknown;
  private abort: AbortController | null = null;
  private run = 0;

  constructor(sources: SearchSource[], timers: Timers = realTimers) {
    this.sources = sources;
    this.timers = timers;
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  snapshot = (): SearchState => this.state;

  private set(s: SearchState): void {
    this.state = s;
    for (const l of [...this.listeners]) l();
  }

  private cancel(): void {
    this.timers.clear(this.timer);
    this.timer = undefined;
    this.abort?.abort();
    this.abort = null;
    this.run++;
  }

  /** The user typed or changed a filter. An empty query clears at once; anything else waits. */
  update(raw: string): void {
    this.cancel();
    const query = normaliseQuery(raw);
    if (query === '') {
      this.set(IDLE);
      return;
    }
    const mine = this.run;
    const ac = new AbortController();
    this.abort = ac;
    const groups: SearchState['groups'] = {};
    const asked: SearchSource[] = [];
    for (const s of this.sources) {
      if ([...query].length >= s.minChars) {
        groups[s.id] = { status: 'searching', rows: this.state.groups[s.id]?.rows ?? [], error: null };
        asked.push(s);
      } else {
        groups[s.id] = { status: 'idle', rows: [], error: null };
      }
    }
    this.set({ query, status: asked.length > 0 ? 'searching' : 'done', groups });
    this.timer = this.timers.set(() => {
      this.timer = undefined;
      for (const s of asked) this.launch(s, query, ac.signal, mine);
    }, DEBOUNCE_MS);
  }

  private launch(source: SearchSource, query: string, signal: AbortSignal, mine: number): void {
    let deadline: unknown;
    const settle = (g: GroupState) => {
      this.timers.clear(deadline);
      if (mine !== this.run) return; // a newer query replaced this one
      const groups = { ...this.state.groups, [source.id]: g };
      const pending = Object.values(groups).some((x) => x?.status === 'searching');
      this.set({ ...this.state, groups, status: pending ? 'searching' : 'done' });
    };
    const timeout = new Promise<never>((_, reject) => {
      deadline = this.timers.set(() => reject(new Error('It took too long.')), source.timeoutMs);
    });
    let answer: Promise<Row[]>;
    try {
      answer = Promise.resolve(source.run(query, signal));
    } catch (e) {
      answer = Promise.reject(e);
    }
    Promise.race([answer, timeout]).then(
      (rows) => settle(settled(rows.slice(0, GROUP_LIMIT[source.id]))),
      (e: unknown) => settle({ status: 'error', rows: [], error: String((e as Error)?.message ?? e) }),
    );
  }

  /** Drop everything pending (the dialog closed). */
  dispose(): void {
    this.cancel();
    this.listeners.clear();
  }
}

/** The rows in display order, each with its group, for keyboard movement across groups. */
export function flatRows(state: SearchState): { group: SourceId; row: Row }[] {
  const present = Object.keys(state.groups) as SourceId[];
  const out: { group: SourceId; row: Row }[] = [];
  for (const g of groupOrder(state.query, present)) {
    for (const row of state.groups[g]?.rows ?? []) out.push({ group: g, row });
  }
  return out;
}

/** Up/Down move the highlight and wrap; with no rows it stays at 0. */
export function moveHighlight(index: number, delta: -1 | 1, count: number): number {
  if (count <= 0) return 0;
  return (index + delta + count) % count;
}

/** PageUp/PageDown move ten rows and stop at the ends. */
export function movePage(index: number, delta: -1 | 1, count: number): number {
  if (count <= 0) return 0;
  return Math.min(count - 1, Math.max(0, index + delta * PAGE_STEP));
}

/** The polite status line: what is happening, or how many results there are. */
export function statusLine(state: SearchState): string {
  if (state.query === '') return 'Type to search.';
  if (state.status === 'searching') return 'Searching…';
  const n = flatRows(state).length;
  return n === 0 ? 'No results.' : n === 1 ? '1 result.' : `${n} results.`;
}
