// The Tasks view's data: the board's projects, its four columns, the live fleet, readiness and the
// card headlines, read with a few separate calls and refreshed by polling (when the tab is shown,
// when the window is focused, every minute while it is shown, and on Refresh). A refresh is
// all-settled: every read runs even if one fails, each column keeps what it did read, and the last
// complete read moves only when everything succeeded. Answers from an older generation are dropped.
// Plain TypeScript, tested under node:test.

import type { Api } from '../api/client.ts';
import { BOARD_COLUMNS, boardOps, type BoardColumnId, type BoardEntry, type BoardProjectRow, type FleetAssignment } from '../api/board.ts';
import { ItemDrawerStore } from './drawer.ts';
import { groupFleet, mergeEntries, readError } from './model.ts';
import { BoardReader, toBoardError } from './reader.ts';

export const COLUMN_PAGE = 30;
export const PROJECT_PAGE = 50;
export const REFRESH_EVERY_MS = 60_000;

export interface ColumnState {
  entries: BoardEntry[];
  total: number | null;
  nextCursor: string | null;
  started: boolean;
  loading: boolean;
  error: string | null;
}

export interface FleetState {
  byItem: ReadonlyMap<string, FleetAssignment[]>;
  complete: boolean;
}

export interface TasksState {
  /** `unknown` until the saved connection has been looked at. */
  connection: 'unknown' | 'ready' | 'missing';
  reading: boolean;
  /** Something has been read at least once. */
  loaded: boolean;
  lastReadAt: number | null;
  /** Board readiness: null when not asked or the answer was unusable. */
  ready: boolean | null;
  /** The sentence for the latest refresh that did not fully succeed. */
  error: string | null;
  /** The latest refresh failed, so what shows is older than it looks. */
  stale: boolean;
  projects: { rows: BoardProjectRow[]; shown: number; loading: boolean; error: string | null; includeCompleted: boolean; selected: string | null };
  columns: Record<BoardColumnId, ColumnState>;
  fleet: FleetState | 'failed' | null;
  /** Bumped by each complete read: card headlines are read again for the new revision. */
  revision: number;
  headlines: Record<string, { revision: number; text: string | null; failed: boolean }>;
  drawerId: string | null;
}

const emptyColumn = (): ColumnState => ({ entries: [], total: null, nextCursor: null, started: false, loading: false, error: null });

function initial(): TasksState {
  return {
    connection: 'unknown',
    reading: false,
    loaded: false,
    lastReadAt: null,
    ready: null,
    error: null,
    stale: false,
    projects: { rows: [], shown: PROJECT_PAGE, loading: false, error: null, includeCompleted: false, selected: null },
    columns: { backlog: emptyColumn(), in_progress: emptyColumn(), waiting: emptyColumn(), completed: emptyColumn() },
    fleet: null,
    revision: 0,
    headlines: {},
    drawerId: null,
  };
}

const isFinished = (p: BoardProjectRow) => typeof p.total === 'number' && p.total > 0 && p.finished === p.total;

/** The projects the rail lists: finished ones only when "Completed projects" is ticked. */
export function visibleProjects(s: TasksState): BoardProjectRow[] {
  return s.projects.rows.filter((p) => s.projects.includeCompleted || !isFinished(p) || p.id === s.projects.selected);
}

export class TasksStore {
  private state: TasksState = initial();
  private readonly listeners = new Set<() => void>();
  private gen = 0;
  private abort: AbortController | null = null;
  private drawerStore: ItemDrawerStore | null = null;
  private readonly ops: ReturnType<typeof boardOps>;
  readonly reader: BoardReader;

  private readonly now: () => number;

  constructor(api: Api, now: () => number = Date.now, reader?: BoardReader) {
    this.now = now;
    this.ops = boardOps(api);
    this.reader = reader ?? new BoardReader(api);
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  snapshot = (): TasksState => this.state;

  get drawer(): ItemDrawerStore | null {
    return this.drawerStore;
  }

  private set(patch: Partial<TasksState>): void {
    this.state = { ...this.state, ...patch };
    for (const l of [...this.listeners]) l();
  }

  private setColumn(col: BoardColumnId, patch: Partial<ColumnState>): void {
    this.set({ columns: { ...this.state.columns, [col]: { ...this.state.columns[col], ...patch } } });
  }

  /** Whether a board address and token are saved; the Tasks entry shows only when they are. */
  async checkConnection(): Promise<boolean> {
    try {
      const c = await this.ops.connection();
      const ok = c.url.trim() !== '' && c.hasToken;
      this.set({ connection: ok ? 'ready' : 'missing' });
      return ok;
    } catch {
      this.set({ connection: 'missing' });
      return false;
    }
  }

  /** Read everything again: projects, fleet, readiness and page one of every column. */
  async refresh(): Promise<void> {
    const mine = ++this.gen;
    this.abort?.abort();
    const ac = new AbortController();
    this.abort = ac;
    const opts = { signal: ac.signal };
    const project = this.state.projects.selected ?? undefined;
    const cols = { ...this.state.columns };
    for (const c of BOARD_COLUMNS) cols[c] = { ...cols[c], loading: true, error: null };
    this.set({ reading: true, columns: cols, projects: { ...this.state.projects, loading: true, error: null } });

    const failures: string[] = [];
    const fail = (e: unknown) => {
      const err = toBoardError(e);
      if (err.code === 'cancelled') return null;
      const text = readError(err.code, err.message);
      failures.push(text);
      return text;
    };
    const live = () => mine === this.gen;

    const projectsRead = this.reader.read({ action: 'projects' }, opts).then(
      (r) => {
        if (live()) this.set({ projects: { ...this.state.projects, rows: r.projects, loading: false, error: null } });
      },
      (e) => {
        const t = fail(e);
        if (live()) this.set({ projects: { ...this.state.projects, loading: false, error: t } });
      },
    );
    const fleetRead = this.reader.read({ action: 'fleet' }, opts).then(
      (r) => {
        if (live()) this.set({ fleet: { byItem: groupFleet(r.assignments), complete: r.complete } });
      },
      (e) => {
        fail(e);
        if (live()) this.set({ fleet: 'failed' });
      },
    );
    let readyNow: boolean | null = null;
    const readyRead = this.reader.read({ action: 'ready' }, opts).then(
      (r) => {
        readyNow = r.ready === true;
      },
      () => {
        readyNow = null;
      },
    );
    const columnReads = BOARD_COLUMNS.map((col) =>
      this.reader.read({ action: 'board_page', column: col, project, limit: COLUMN_PAGE }, opts).then(
        (r) => {
          const s = r.board.columns[col];
          if (live()) this.setColumn(col, { entries: s?.entries ?? [], total: s?.total ?? null, nextCursor: s?.nextCursor ?? null, started: true, loading: false, error: null });
        },
        (e) => {
          const t = fail(e);
          if (live()) this.setColumn(col, { loading: false, error: t, started: true });
        },
      ),
    );
    await Promise.all([projectsRead, fleetRead, readyRead, ...columnReads]);
    if (!live()) return;
    const ok = failures.length === 0;
    this.set({
      reading: false,
      loaded: true,
      ready: readyNow,
      error: ok ? null : (failures[0] ?? null),
      stale: !ok,
      ...(ok ? { lastReadAt: this.now(), revision: this.state.revision + 1 } : {}),
    });
  }

  /** Stop what is in flight (the tab is hidden); answers that still arrive are dropped. */
  pause(): void {
    this.gen++;
    this.abort?.abort();
    this.abort = null;
    if (this.state.reading) {
      const cols = { ...this.state.columns };
      for (const c of BOARD_COLUMNS) cols[c] = { ...cols[c], loading: false };
      this.set({ reading: false, columns: cols, projects: { ...this.state.projects, loading: false } });
    }
  }

  async loadMore(col: BoardColumnId): Promise<void> {
    const cur = this.state.columns[col];
    if (cur.loading || !cur.nextCursor) return;
    const mine = this.gen;
    this.setColumn(col, { loading: true, error: null });
    try {
      const r = await this.reader.read({ action: 'board_page', column: col, project: this.state.projects.selected ?? undefined, limit: COLUMN_PAGE, cursor: cur.nextCursor });
      if (mine !== this.gen) return;
      const s = r.board.columns[col];
      const now = this.state.columns[col];
      this.setColumn(col, { entries: mergeEntries(now.entries, s?.entries ?? []), total: s?.total ?? now.total, nextCursor: s?.nextCursor ?? null, loading: false });
    } catch (e) {
      if (mine !== this.gen) return;
      const err = toBoardError(e);
      this.setColumn(col, { loading: false, error: readError(err.code, err.message) });
    }
  }

  selectProject(id: string | null): void {
    if (this.state.projects.selected === id) return;
    this.closeDrawer();
    const cols = { ...this.state.columns };
    for (const c of BOARD_COLUMNS) cols[c] = emptyColumn();
    this.set({ projects: { ...this.state.projects, selected: id }, columns: cols });
    void this.refresh();
  }

  setIncludeCompleted(on: boolean): void {
    // Changing the filter also goes back to All projects.
    this.set({ projects: { ...this.state.projects, includeCompleted: on, shown: PROJECT_PAGE } });
    this.selectProject(null);
  }

  showMoreProjects(): void {
    this.set({ projects: { ...this.state.projects, shown: this.state.projects.shown + PROJECT_PAGE } });
  }

  /** A card's latest checkpoint headline, read once per snapshot revision. */
  async loadHeadline(itemId: string): Promise<void> {
    const rev = this.state.revision;
    const cur = this.state.headlines[itemId];
    if (cur && cur.revision === rev) return;
    this.set({ headlines: { ...this.state.headlines, [itemId]: { revision: rev, text: null, failed: false } } });
    try {
      const r = await this.reader.read({ action: 'item', id: itemId });
      this.set({ headlines: { ...this.state.headlines, [itemId]: { revision: rev, text: r.item.checkpointHeadline ?? null, failed: false } } });
    } catch (e) {
      if (toBoardError(e).code === 'cancelled') return;
      this.set({ headlines: { ...this.state.headlines, [itemId]: { revision: rev, text: null, failed: true } } });
    }
  }

  openDrawer(itemId: string): void {
    if (this.drawerStore?.id === itemId) return;
    this.drawerStore?.close();
    this.drawerStore = new ItemDrawerStore(this.reader, itemId);
    this.drawerStore.start();
    this.set({ drawerId: itemId });
  }

  closeDrawer(): void {
    if (!this.drawerStore) return;
    this.drawerStore.close();
    this.drawerStore = null;
    this.set({ drawerId: null });
  }
}
