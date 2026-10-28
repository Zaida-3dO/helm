// One cache entry per open pull request: the header poll, each view's data, the typed-but-unsent
// text and the per-PR write queue. The tab's body unmounts when hidden; this survives it and goes
// away with the tab. Loads page only while the tab is shown and the window focused, and a load that
// is interrupted stores nothing.

import type { Api } from '../api/client.ts';
import { prClient, errorText } from './client.ts';
import type { PrClient } from './client.ts';
import {
  arr,
  buildTimeline,
  gqlPr,
  pageInfo,
  parseChecks,
  parseCommits,
  parseFiles,
  parseIssueComments,
  parseReviews,
  parseSummary,
  parseThreads,
  parseTimelineEvents,
  parseViewed,
} from './parse.ts';
import { prKey } from './links.ts';
import type { CheckRow, Commit, LandInfo, PrFile, PrRef, PrSummary, PrView, Revision, Thread, TimelineItem, ViewedState } from './types.ts';

export const POLL_MS = 30_000;
const MAX_PAGES = 10;
const MAX_FILE_PAGES = 30;
const MAX_COMMIT_PAGES = 3;
export const FILE_CAP = 3000;

export interface Loaded<T> {
  data: T;
  at: number;
  /** The list was cut at the page ceiling. */
  truncated: boolean;
}

export interface FilesData {
  files: PrFile[];
  viewed: Map<string, ViewedState>;
  headSha: string;
  baseSha: string;
}

export class PrEntry {
  readonly key: string;
  readonly ref: PrRef;
  readonly client: PrClient;
  version = 0;
  closed = false;

  // what the tab shows right now (set by the body on every render)
  shown = { visible: false, focused: false, view: 'conversation' as PrView };

  summary: PrSummary | null = null;
  revision: Revision | null = null;
  summaryError: string | null = null;
  lastAttempt = 0;
  summaryAt = 0;

  conv: Loaded<TimelineItem[]> | null = null;
  convError: string | null = null;
  convLoading = false;
  convStale = false;
  convUpdatedAt = '';
  threads: Loaded<Thread[]> | null = null;
  commits: Loaded<Commit[]> | null = null;
  commitsError: string | null = null;
  commitsHead = '';
  checks: Loaded<CheckRow[]> | null = null;
  checksError: string | null = null;
  checksAttempt = 0;
  files: Loaded<FilesData> | null = null;
  filesError: string | null = null;
  filesLoading = false;
  newCommits = false;
  land: LandInfo | null = null;
  landError: string | null = null;

  /** Switches the tab to another view (set by the tab body). */
  goto: (view: PrView) => void = () => undefined;

  /** Typed text not yet sent, by box (comment, reply:<thread>, review, line:<path>:<side>:<line>). */
  drafts = new Map<string, string>();
  sending = new Set<string>();
  outcomes = new Map<string, { kind: 'ok' | 'refused' | 'uncertain'; text: string; cls?: string }>();
  /** Per-view memory that should survive a view switch. */
  ui: Record<string, unknown> = {};

  private listeners = new Set<() => void>();
  private inflight = new Map<string, Promise<void>>();
  private queue: Promise<unknown> = Promise.resolve();
  private convLoadedAt = 0;

  constructor(ref: PrRef, api: Api) {
    this.ref = ref;
    this.key = prKey(ref);
    this.client = prClient(api);
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  getVersion = (): number => this.version;

  changed(): void {
    this.version++;
    for (const l of [...this.listeners]) l();
  }

  setShown(visible: boolean, focused: boolean, view: PrView): void {
    this.shown = { visible, focused, view };
  }

  /** A loader may continue only while the tab is open, shown and (unless told otherwise) focused. */
  private live(view: PrView | null, needFocus = true): boolean {
    if (this.closed || !this.shown.visible) return false;
    if (needFocus && !this.shown.focused) return false;
    return view === null || this.shown.view === view;
  }

  private once(name: string, run: () => Promise<void>): Promise<void> {
    const have = this.inflight.get(name);
    if (have) return have;
    const p = run().finally(() => this.inflight.delete(name));
    this.inflight.set(name, p);
    return p;
  }

  /** Runs one write after the previous write on this PR has finished, so a write that reads first sees the one before it. */
  enqueue<T>(run: () => Promise<T>): Promise<T> {
    const next = this.queue.then(run, run);
    this.queue = next.catch(() => undefined);
    return next;
  }

  // ---- the header poll ----

  /** Polls when the last attempt (good or bad) is at least one interval old. */
  pollIfStale(now = Date.now()): Promise<void> {
    return now - this.lastAttempt >= POLL_MS ? this.poll() : Promise.resolve();
  }

  poll(): Promise<void> {
    return this.once('poll', async () => {
      this.lastAttempt = Date.now();
      try {
        const [pull, sum] = await Promise.all([this.client.pull(this.ref), this.client.summary(this.ref)]);
        if (this.closed) return;
        const s = parseSummary(pull.data, sum.data);
        const rev: Revision = { headSha: s.headSha, baseSha: s.baseSha, updatedAt: s.updatedAt };
        const prev = this.revision;
        this.summary = s;
        this.revision = rev;
        this.summaryError = null;
        this.summaryAt = Date.now();
        if (prev && this.files && (this.files.data.headSha !== rev.headSha || this.files.data.baseSha !== rev.baseSha)) this.newCommits = true;
        if (this.conv && this.convUpdatedAt && rev.updatedAt !== this.convUpdatedAt) {
          this.convStale = true;
          // Reload the timeline only when it is on screen and not just reloaded.
          if (this.live('conversation') && Date.now() - this.convLoadedAt >= POLL_MS) void this.loadConversation();
        }
        if (this.shown.view === 'commits' && this.commits && this.commitsHead !== rev.headSha && this.live('commits')) void this.loadCommits();
      } catch (e) {
        if (this.closed) return;
        this.summaryError = errorText(e);
      }
      this.changed();
    });
  }

  // ---- Conversation ----

  loadConversation(force = false): Promise<void> {
    return this.once('conv', async () => {
      if (!force && this.conv && !this.convStale) return;
      this.convLoading = true;
      this.convError = null;
      this.changed();
      try {
        const marker = this.revision?.updatedAt ?? '';
        const [events, comments, reviews, threads] = await Promise.all([
          this.timelineEvents(),
          this.restPages((p) => this.client.issueComments(this.ref, p), parseIssueComments),
          this.restPages((p) => this.client.reviews(this.ref, p), parseReviews),
          this.threadRound(),
        ]);
        if (this.closed) return;
        if (!events || !comments || !reviews || !threads) {
          this.convLoading = false;
          this.changed();
          return;
        }
        const items = buildTimeline({ comments: comments.items, reviews: reviews.items, events: events.items, threads: threads.items });
        this.conv = { data: items, at: Date.now(), truncated: events.truncated || comments.truncated || reviews.truncated };
        this.threads = { data: threads.items, at: Date.now(), truncated: threads.truncated };
        this.convUpdatedAt = marker;
        this.convStale = false;
        this.convLoadedAt = Date.now();
        void this.loadLandInfo();
      } catch (e) {
        if (!this.closed) this.convError = errorText(e);
      }
      this.convLoading = false;
      this.changed();
    });
  }

  /** Null when the round was interrupted (tab hidden or unfocused): nothing is stored. */
  private async timelineEvents(): Promise<{ items: TimelineItem[]; truncated: boolean } | null> {
    const items: TimelineItem[] = [];
    let after: string | null = null;
    for (let i = 0; i < MAX_PAGES; i++) {
      if (i > 0 && !this.live('conversation')) return null;
      const a = await this.client.timeline(this.ref, after);
      const conn = gqlPr(a.data).timelineItems;
      items.push(...parseTimelineEvents(conn));
      const p = pageInfo(conn);
      if (!p.hasNext) return { items, truncated: false };
      after = p.end;
    }
    return { items, truncated: true };
  }

  private async restPages<T>(fetchPage: (page: number) => Promise<{ data: unknown; nextPage?: number | null }>, read: (list: unknown) => T[]): Promise<{ items: T[]; truncated: boolean } | null> {
    const items: T[] = [];
    let page = 1;
    for (let i = 0; i < MAX_PAGES; i++) {
      if (i > 0 && !this.live('conversation')) return null;
      const a = await fetchPage(page);
      items.push(...read(a.data));
      if (!a.nextPage) return { items, truncated: false };
      page = a.nextPage;
    }
    return { items, truncated: true };
  }

  /** The shared review-thread round: Conversation and the inline threads in Files read it. */
  private async threadRound(): Promise<{ items: Thread[]; truncated: boolean } | null> {
    const items: Thread[] = [];
    let after: string | null = null;
    for (let i = 0; i < MAX_PAGES; i++) {
      if (i > 0 && !this.live(null)) return null;
      const a = await this.client.threads(this.ref, after);
      const conn = gqlPr(a.data).reviewThreads;
      items.push(...parseThreads(conn));
      const p = pageInfo(conn);
      if (!p.hasNext) return { items, truncated: false };
      after = p.end;
    }
    return { items, truncated: true };
  }

  /** Reads the threads alone (Files, and after a write that changes only threads). */
  loadThreads(): Promise<void> {
    return this.once('threads', async () => {
      try {
        const r = await this.threadRound();
        if (!r || this.closed) return;
        this.threads = { data: r.items, at: Date.now(), truncated: r.truncated };
        if (this.conv) this.conv = { ...this.conv, data: this.replaceThreads(this.conv.data, r.items) };
      } catch {
        // Threads are an extra on the Files view: keep the previous snapshot.
      }
      this.changed();
    });
  }

  private replaceThreads(items: TimelineItem[], threads: Thread[]): TimelineItem[] {
    const rest = items.filter((i) => i.type !== 'thread').map((i) => (i.type === 'review' ? { ...i, threads: [] } : i)) as TimelineItem[];
    const reviews = rest.filter((i): i is Extract<TimelineItem, { type: 'review' }> => i.type === 'review');
    const events = rest.filter((i) => i.type === 'event' || i.type === 'commit');
    const comments = rest.filter((i) => i.type === 'comment');
    return buildTimeline({ comments, reviews, events, threads });
  }

  /** Updates one thread in place with no re-read (resolve and unresolve). */
  patchThread(id: string, patch: Partial<Thread>): void {
    const swap = (t: Thread) => (t.id === id ? { ...t, ...patch } : t);
    if (this.threads) this.threads = { ...this.threads, data: this.threads.data.map(swap) };
    if (this.conv) {
      this.conv = {
        ...this.conv,
        data: this.conv.data.map((i) => (i.type === 'review' ? { ...i, threads: i.threads.map(swap) } : i.type === 'thread' ? { ...i, thread: swap(i.thread) } : i)),
      };
    }
    this.changed();
  }

  loadLandInfo(): Promise<void> {
    return this.once('land', async () => {
      if (this.summary?.state !== 'OPEN') return;
      try {
        this.land = await this.client.landInfo(this.ref, this.summary?.headRef);
        this.landError = null;
      } catch (e) {
        this.landError = errorText(e);
      }
      this.changed();
    });
  }

  // ---- Commits ----

  loadCommits(force = false): Promise<void> {
    return this.once('commits', async () => {
      if (!force && this.commits && this.commitsHead === (this.revision?.headSha ?? '')) return;
      this.commitsError = null;
      try {
        const items: Commit[] = [];
        let after: string | null = null;
        let truncated = true;
        for (let i = 0; i < MAX_COMMIT_PAGES; i++) {
          if (i > 0 && !this.live('commits')) return;
          const a = await this.client.commits(this.ref, after);
          const conn = gqlPr(a.data).commits;
          items.push(...parseCommits(conn));
          const p = pageInfo(conn);
          if (!p.hasNext) {
            truncated = false;
            break;
          }
          after = p.end;
        }
        if (this.closed) return;
        this.commits = { data: items, at: Date.now(), truncated };
        this.commitsHead = this.revision?.headSha ?? '';
      } catch (e) {
        if (!this.closed) this.commitsError = errorText(e);
      }
      this.changed();
    });
  }

  // ---- Checks ----

  /** True when something is still pending: in the list, in the summary's tally, or never loaded. */
  checksPending(): boolean {
    if (!this.checks) return true;
    if (this.checks.data.some((c) => c.group === 'pending')) return true;
    return this.summary?.checksState === 'PENDING' || this.summary?.checksState === 'EXPECTED';
  }

  loadChecks(): Promise<void> {
    return this.once('checks', async () => {
      this.checksAttempt = Date.now();
      this.checksError = null;
      try {
        const items: CheckRow[] = [];
        let after: string | null = null;
        let truncated = true;
        for (let i = 0; i < MAX_COMMIT_PAGES; i++) {
          if (i > 0 && !this.live('checks')) return;
          const a = await this.client.checks(this.ref, after);
          const node = gqlPr(a.data).commits as { nodes?: unknown[] } | undefined;
          const contexts = (arr(node?.nodes)[0] as { commit?: { statusCheckRollup?: { contexts?: unknown } } } | undefined)?.commit?.statusCheckRollup?.contexts;
          items.push(...parseChecks(contexts));
          const p = pageInfo(contexts);
          if (!p.hasNext) {
            truncated = false;
            break;
          }
          after = p.end;
        }
        if (this.closed) return;
        this.checks = { data: items, at: Date.now(), truncated };
      } catch (e) {
        if (!this.closed) this.checksError = errorText(e);
      }
      this.changed();
    });
  }

  // ---- Files changed ----

  loadFiles(): Promise<void> {
    return this.once('files', async () => {
      this.filesLoading = true;
      this.filesError = null;
      this.changed();
      try {
        const head = this.summary?.headSha ?? this.revision?.headSha ?? '';
        const base = this.summary?.baseSha ?? '';
        const files: PrFile[] = [];
        let page = 1;
        let truncated = true;
        for (let i = 0; i < MAX_FILE_PAGES; i++) {
          if (i > 0 && !this.live('files')) {
            this.filesLoading = false;
            this.changed();
            return;
          }
          const a = await this.client.files(this.ref, page);
          files.push(...parseFiles(a.data));
          if (!a.nextPage || files.length >= FILE_CAP) {
            truncated = Boolean(a.nextPage);
            break;
          }
          page = a.nextPage;
        }
        const viewed = new Map<string, ViewedState>();
        let after: string | null = null;
        for (let i = 0; i < MAX_FILE_PAGES; i++) {
          if (i > 0 && !this.live('files')) break;
          const a = await this.client.viewed(this.ref, after);
          const conn = gqlPr(a.data).files;
          for (const [k, v] of parseViewed(conn)) viewed.set(k, v);
          const p = pageInfo(conn);
          if (!p.hasNext) break;
          after = p.end;
        }
        if (this.closed) return;
        this.files = { data: { files, viewed, headSha: head, baseSha: base }, at: Date.now(), truncated };
        this.newCommits = false;
        void this.loadThreads();
      } catch (e) {
        if (!this.closed) this.filesError = errorText(e);
      }
      this.filesLoading = false;
      this.changed();
    });
  }

  setViewed(path: string, state: ViewedState): void {
    if (!this.files) return;
    const viewed = new Map(this.files.data.viewed);
    viewed.set(path, state);
    this.files = { ...this.files, data: { ...this.files.data, viewed } };
    this.changed();
  }

  // ---- refresh ----

  /** Refresh: a poll that starts now, then the shown view reloads at the new revision. */
  async refresh(): Promise<'updated' | 'failed' | 'skipped'> {
    if (!this.shown.visible || !this.shown.focused) return 'skipped';
    await this.poll();
    if (this.summaryError) return 'failed';
    if (this.closed || !this.shown.visible) return 'updated';
    switch (this.shown.view) {
      case 'conversation':
        await this.loadConversation(true);
        break;
      case 'commits':
        await this.loadCommits(true);
        break;
      case 'checks':
        await this.loadChecks();
        break;
      case 'files':
        await this.loadFiles();
        break;
    }
    return 'updated';
  }

  /** The oldest data on screen: the header poll or the shown view's own load. */
  oldestAt(): number {
    const view =
      this.shown.view === 'conversation' ? this.conv?.at : this.shown.view === 'commits' ? this.commits?.at : this.shown.view === 'checks' ? this.checks?.at : this.files?.at;
    return Math.min(this.summaryAt || Infinity, view ?? Infinity);
  }

  close(): void {
    this.closed = true;
    this.listeners.clear();
  }
}

const entries = new Map<string, PrEntry>();

export function getEntry(ref: PrRef, api: Api): PrEntry {
  const key = prKey(ref);
  let e = entries.get(key);
  if (!e || e.closed) {
    e = new PrEntry(ref, api);
    entries.set(key, e);
  }
  return e;
}

export function dropEntry(ref: PrRef): void {
  const key = prKey(ref);
  entries.get(key)?.close();
  entries.delete(key);
}

export function peekEntry(ref: PrRef): PrEntry | undefined {
  return entries.get(prKey(ref));
}
