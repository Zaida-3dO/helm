// The sidebar's data holder: projects and the chat rows the window holds, per-view paging, and
// the event wiring (the `project`, `sidebar`, `board_sync` and `chat` events).
// React reads it through useSyncExternalStore; every change replaces `snapshot`.

import type { EventBus } from '../api/events.ts';
import type { ProjectNode, ProjectOps, SidebarRow, SidebarView, BoardSyncState } from '../api/projects.ts';
import type { AttentionSummary, ChatRecord } from '../api/types.ts';
import { rowAttention } from './status.ts';
import { loadMore } from './paging.ts';

export interface ViewPaging {
  cursor: string | null;
  complete: boolean;
  loading: boolean;
  error: string | null;
  started: boolean;
}

export interface SidebarSnapshot {
  projects: ProjectNode[];
  rows: SidebarRow[];
  paging: Record<SidebarView, ViewPaging>;
  sync: BoardSyncState | null;
  error: string | null;
  needsYou: ReadonlySet<string>;
  /** Quiet chats the idle check thinks still need you, by chat id, with its reason. */
  flagged: ReadonlyMap<string, string>;
  /** The whole attention summary (null until the first read), for the nav count, the Workspace
   *  status popover and the Needs attention inbox. */
  attention: AttentionSummary | null;
}

const freshPaging = (): ViewPaging => ({ cursor: null, complete: false, loading: false, error: null, started: false });

export class SidebarData {
  private projects = new Map<string, ProjectNode>();
  private projectOrder: string[] = [];
  private rows = new Map<string, SidebarRow>();
  private paging: Record<SidebarView, ViewPaging> = {
    projects: freshPaging(),
    activity: freshPaging(),
    archived: freshPaging(),
  };
  private sync: BoardSyncState | null = null;
  private needsYou: ReadonlySet<string> = new Set();
  private flagged: ReadonlyMap<string, string> = new Map();
  private summary: AttentionSummary | null = null;
  private error: string | null = null;
  private listeners = new Set<() => void>();
  snapshot: SidebarSnapshot = this.make();

  private readonly ops: ProjectOps;

  constructor(ops: ProjectOps) {
    this.ops = ops;
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  getSnapshot = (): SidebarSnapshot => this.snapshot;

  private make(): SidebarSnapshot {
    return {
      projects: this.projectOrder.map((id) => this.projects.get(id)!).filter(Boolean),
      rows: [...this.rows.values()],
      paging: { ...this.paging },
      sync: this.sync,
      error: this.error,
      needsYou: this.needsYou,
      flagged: this.flagged,
      attention: this.summary,
    };
  }

  private emit(): void {
    this.snapshot = this.make();
    for (const l of this.listeners) l();
  }

  async loadProjects(): Promise<void> {
    try {
      const list = await this.ops.list(true);
      this.projects = new Map(list.map((p) => [p.id, p]));
      this.projectOrder = list.map((p) => p.id);
      this.error = null;
    } catch (e) {
      this.error = errorText(e);
    }
    this.emit();
  }

  project(p: ProjectNode): void {
    const prev = this.projects.get(p.id);
    if (prev && prev.revision > p.revision) return;
    this.projects.set(p.id, p);
    // Order or nesting may have changed: re-read the list for the host's order.
    void this.loadProjects();
  }

  mergeRows(rows: SidebarRow[]): void {
    let unknownGroup = false;
    for (const r of rows) {
      const prev = this.rows.get(r.chatId);
      if (!prev || prev.revision <= r.revision) this.rows.set(r.chatId, r);
      if (!this.projects.has(r.groupProjectId)) unknownGroup = true;
    }
    this.emit();
    // A new chat can create its folder project (chat_create), which sends no project event.
    if (unknownGroup && !this.reloading) {
      this.reloading = true;
      void this.loadProjects().finally(() => (this.reloading = false));
    }
  }

  private reloading = false;

  /** A chat record from the `chat` event: patch what the row shows, or fetch a new row. */
  chat(c: ChatRecord): void {
    const prev = this.rows.get(c.id);
    if (!prev || prev.archived !== c.archived || prev.parentChatId !== c.parentChatId || c.sideSourceId) {
      void this.refetch([c.id]);
      return;
    }
    const unread = c.unread.markToken !== null || c.unread.completionId !== null;
    const next: SidebarRow = {
      ...prev,
      title: c.title,
      status: c.status,
      updatedAt: c.updatedAt,
      pinned: c.pinned,
      unread,
      muted: c.muted,
      stalled: c.stalled,
      hasDraft: c.draft.trim() !== '',
      lastActivityAt: c.lastActivityAt,
    };
    next.attention = rowAttention({ status: c.status, unread, lingering: false, pinned: c.pinned });
    if (JSON.stringify(next) === JSON.stringify(prev)) return;
    this.rows.set(c.id, next);
    this.emit();
  }

  /** Chats waiting on the user, from the attention summary. */
  attention(summary: AttentionSummary): void {
    this.needsYou = new Set(summary.needsYou.map((r) => r.chatId));
    this.flagged = new Map(summary.flagged.map((r) => [r.chatId, r.reason]));
    this.summary = summary;
    this.emit();
  }

  async refetch(chatIds: string[]): Promise<void> {
    for (let i = 0; i < chatIds.length; i += 200) {
      const ids = chatIds.slice(i, i + 200);
      try {
        const rows = await this.ops.rows(ids);
        const got = new Set(rows.map((r) => r.chatId));
        for (const id of ids) if (!got.has(id)) this.rows.delete(id);
        this.mergeRows(rows);
      } catch (e) {
        this.error = errorText(e);
        this.emit();
      }
    }
  }

  /** The first page of a view (once), or one "Load more chats" click. */
  async page(view: SidebarView): Promise<void> {
    const p = this.paging[view];
    if (p.loading || p.complete) return;
    this.paging[view] = { ...p, loading: true, error: null };
    this.emit();
    try {
      const r = await loadMore<SidebarRow>({
        cursor: p.started ? p.cursor : null,
        held: (row) => this.rows.has(row.chatId),
        heldCount: this.rows.size,
        fetch: (cursor) => this.ops.page(view, cursor ?? undefined),
        merge: (rows) => this.mergeRows(rows),
      });
      this.paging[view] = { cursor: r.cursor, complete: r.complete, loading: false, error: null, started: true };
    } catch (e) {
      this.paging[view] = { ...this.paging[view], loading: false, error: errorText(e) };
    }
    this.emit();
  }

  /** Changes that move chats in paging order send unfinished views back to the start. */
  invalidatePaging(): void {
    for (const v of ['projects', 'activity', 'archived'] as SidebarView[]) {
      if (!this.paging[v].complete) this.paging[v] = freshPaging();
    }
    this.emit();
  }

  setSync(s: BoardSyncState): void {
    this.sync = s;
    this.emit();
  }

  row(chatId: string): SidebarRow | undefined {
    return this.rows.get(chatId);
  }

  connect(bus: EventBus): () => void {
    const offs = [
      bus.on('project', (e) => this.project(e.project as ProjectNode)),
      bus.on('sidebar', (e) => void this.refetch((e.chatIds as string[]) ?? [])),
      bus.on('chat', (e) => this.chat(e.chat as ChatRecord)),
      bus.on('attention', (e) => this.attention(e.summary as AttentionSummary)),
      bus.on('chat_removed', (e) => void this.refetch([String(e.chatId)])),
      bus.on('board_sync', (e) => {
        this.setSync(e.state as BoardSyncState);
        void this.loadProjects();
      }),
    ];
    return () => offs.forEach((o) => o());
  }
}

export function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}
