// The item drawer's data: the item itself, its latest checkpoint, and three sections read a page at
// a time (history and checkpoints, evidence, open loops), 20 to a page. A section that fails keeps
// what it has and offers Retry. Opening another item replaces everything, and answers that arrive
// for a closed item are dropped. Plain TypeScript, tested under node:test.

import type { ArtifactEntry, HistoryEntry, ItemRecordLite, LoopEntry } from '../api/board.ts';
import { toBoardError, type BoardReader } from './reader.ts';
import { readError } from './model.ts';

export const SECTION_PAGE = 20;

export interface Section<T> {
  items: T[];
  total: number | null;
  nextCursor: string | null;
  started: boolean;
  loading: boolean;
  error: string | null;
}

const emptySection = <T,>(): Section<T> => ({ items: [], total: null, nextCursor: null, started: false, loading: false, error: null });

export interface DrawerState {
  id: string;
  loading: boolean;
  error: string | null;
  item: ItemRecordLite | null;
  checkpoint: string | null;
  history: Section<HistoryEntry>;
  evidence: Section<ArtifactEntry>;
  loops: Section<LoopEntry>;
  /** Full text of loops the reader asked for, by loop id. */
  loopText: Record<string, string>;
}

type SectionKey = 'history' | 'evidence' | 'loops';

export class ItemDrawerStore {
  private state: DrawerState;
  private readonly listeners = new Set<() => void>();
  private closed = false;
  private readonly abort = new AbortController();

  private readonly reader: BoardReader;
  readonly id: string;

  constructor(reader: BoardReader, id: string) {
    this.reader = reader;
    this.id = id;
    this.state = { id, loading: true, error: null, item: null, checkpoint: null, history: emptySection(), evidence: emptySection(), loops: emptySection(), loopText: {} };
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  snapshot = (): DrawerState => this.state;

  private set(patch: Partial<DrawerState>): void {
    if (this.closed) return;
    this.state = { ...this.state, ...patch };
    for (const l of [...this.listeners]) l();
  }

  /** Stop: pending reads are cancelled and their answers dropped. */
  close(): void {
    this.closed = true;
    this.abort.abort();
    this.listeners.clear();
  }

  /** Read the item, its checkpoint headline and the first page of every section. */
  start(): void {
    void this.loadItem();
    for (const k of ['history', 'evidence', 'loops'] as const) void this.loadSection(k);
  }

  async loadItem(): Promise<void> {
    this.set({ loading: true, error: null });
    const opts = { signal: this.abort.signal };
    const [detail, summary] = await Promise.allSettled([
      this.reader.read({ action: 'detail', id: this.id, historyLimit: 1, artifactLimit: 1 }, opts),
      this.reader.read({ action: 'item', id: this.id }, opts),
    ]);
    if (detail.status === 'rejected') {
      const e = toBoardError(detail.reason);
      this.set({ loading: false, error: readError(e.code, e.message) });
      return;
    }
    this.set({
      loading: false,
      item: detail.value.detail.item,
      checkpoint: summary.status === 'fulfilled' ? (summary.value.item.checkpointHeadline ?? null) : null,
    });
  }

  /** The next page of a section (the first when nothing is loaded). */
  async loadSection(key: SectionKey): Promise<void> {
    const cur = this.state[key] as Section<unknown>;
    if (cur.loading || (cur.started && cur.nextCursor === null && cur.error === null)) return;
    this.set({ [key]: { ...cur, loading: true, error: null } } as Partial<DrawerState>);
    const cursor = cur.nextCursor ?? undefined;
    const opts = { signal: this.abort.signal };
    try {
      let items: unknown[];
      let total: number;
      let next: string | null;
      if (key === 'history') {
        const r = await this.reader.read({ action: 'history', id: this.id, limit: SECTION_PAGE, cursor }, opts);
        [items, total, next] = [r.entries, r.total, r.nextCursor];
      } else if (key === 'evidence') {
        const r = await this.reader.read({ action: 'artifacts', id: this.id, limit: SECTION_PAGE, cursor }, opts);
        [items, total, next] = [r.artifacts, r.total, r.nextCursor];
      } else {
        const r = await this.reader.read({ action: 'loops', id: this.id, limit: SECTION_PAGE, cursor }, opts);
        [items, total, next] = [r.loops, r.total, r.nextCursor];
      }
      const now = this.state[key] as Section<unknown>;
      this.set({ [key]: { items: [...now.items, ...items], total, nextCursor: next, started: true, loading: false, error: null } } as Partial<DrawerState>);
    } catch (e) {
      const err = toBoardError(e);
      if (err.code === 'cancelled') return;
      const now = this.state[key] as Section<unknown>;
      this.set({ [key]: { ...now, started: true, loading: false, error: readError(err.code, err.message) } } as Partial<DrawerState>);
    }
  }

  /** A loop's preview was cut; read its full text. */
  async loadLoopText(loopId: string): Promise<void> {
    if (this.state.loopText[loopId] !== undefined) return;
    try {
      const r = await this.reader.read({ action: 'loop', id: this.id, loopId }, { signal: this.abort.signal });
      this.set({ loopText: { ...this.state.loopText, [loopId]: r.text } });
    } catch (e) {
      const err = toBoardError(e);
      if (err.code !== 'cancelled') this.set({ loops: { ...this.state.loops, error: readError(err.code, err.message) } });
    }
  }
}
