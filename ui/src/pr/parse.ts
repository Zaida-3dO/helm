// Reads GitHub's answers into the plain shapes in types.ts. Every reader is defensive: a field that
// is missing or the wrong type becomes an empty value, never a throw, so one odd node cannot
// blank a whole view. Avatar addresses are never read (people are drawn as initials).

import type {
  CheckGroup,
  CheckRow,
  ChecksState,
  Commit,
  FileStatus,
  Label,
  PrFile,
  PrState,
  PrSummary,
  ReviewDecision,
  ReviewState,
  Thread,
  ThreadComment,
  TimelineItem,
  ViewedState,
} from './types.ts';

type Obj = Record<string, unknown>;

export const obj = (x: unknown): Obj => (x && typeof x === 'object' && !Array.isArray(x) ? (x as Obj) : {});
export const arr = (x: unknown): unknown[] => (Array.isArray(x) ? x : []);
export const str = (x: unknown): string => (typeof x === 'string' ? x : '');
export const num = (x: unknown): number => (typeof x === 'number' && Number.isFinite(x) ? x : 0);

const login = (x: unknown): string => str(obj(x).login) || str(obj(obj(x).user).login) || str(obj(x).name);

/** A label colour is used only when it is exactly six hex digits. */
export function labelColor(raw: unknown): string | null {
  const s = str(raw);
  return /^[0-9a-fA-F]{6}$/.test(s) ? `#${s.toLowerCase()}` : null;
}

function nodes(conn: unknown): unknown[] {
  const c = obj(conn);
  return arr(c.nodes).length ? arr(c.nodes) : arr(c.edges).map((e) => obj(e).node);
}

/** The pull request node of a GraphQL answer's data (`repository.pullRequest`). */
export function gqlPr(data: unknown): Obj {
  const d = obj(data);
  const inner = obj(d.data).repository ? obj(d.data) : d;
  return obj(obj(inner.repository).pullRequest);
}

export function pageInfo(conn: unknown): { hasNext: boolean; end: string | null } {
  const p = obj(obj(conn).pageInfo);
  return { hasNext: p.hasNextPage === true, end: str(p.endCursor) || null };
}

const DECISIONS = new Set(['APPROVED', 'CHANGES_REQUESTED', 'REVIEW_REQUIRED']);
const CHECK_STATES = new Set(['SUCCESS', 'FAILURE', 'ERROR', 'PENDING', 'EXPECTED']);

/** One header poll: the REST pull request plus the summary document. */
export function parseSummary(rest: unknown, summary: unknown): PrSummary {
  const r = obj(rest);
  const gp = gqlPr(summary);
  const merged = r.merged === true || Boolean(r.merged_at);
  const state: PrState = merged ? 'MERGED' : str(r.state).toLowerCase() === 'closed' ? 'CLOSED' : 'OPEN';
  const rollup = obj(obj(obj(arr(obj(gp.commits).nodes)[0]).commit).statusCheckRollup ?? gp.statusCheckRollup);
  const decision = str(gp.reviewDecision);
  const cs = str(rollup.state);
  const head = obj(r.head);
  const base = obj(r.base);
  return {
    nodeId: str(gp.id) || str(r.node_id) || null,
    title: str(r.title),
    body: str(r.body),
    state,
    draft: r.draft === true,
    author: login(r.user),
    authorAssociation: str(r.author_association),
    baseRef: str(base.ref),
    headRef: str(head.ref),
    baseSha: str(base.sha),
    headSha: str(head.sha),
    commits: num(r.commits),
    comments: num(r.comments) + num(r.review_comments),
    changedFiles: num(r.changed_files),
    additions: num(r.additions),
    deletions: num(r.deletions),
    createdAt: str(r.created_at),
    updatedAt: str(r.updated_at),
    mergedAt: str(r.merged_at) || null,
    mergedBy: r.merged_by ? login(r.merged_by) || null : null,
    mergeableState: str(r.mergeable_state) || 'unknown',
    labels: arr(r.labels).map((l): Label => ({ name: str(obj(l).name), color: labelColor(obj(l).color) })),
    reviewers: arr(r.requested_reviewers).map(login).filter(Boolean),
    assignees: arr(r.assignees).map(login).filter(Boolean),
    reviewDecision: DECISIONS.has(decision) ? (decision as ReviewDecision) : null,
    checksState: CHECK_STATES.has(cs) ? (cs as ChecksState) : null,
    checksTotal: num(obj(rollup.contexts).totalCount),
    viewer: str(obj(obj(obj(summary).data).viewer ?? obj(summary).viewer).login) || null,
  };
}

// ---- Conversation ----

export function parseIssueComments(list: unknown): TimelineItem[] {
  return arr(list).map((c) => {
    const o = obj(c);
    const created = str(o.created_at);
    return {
      type: 'comment' as const,
      key: `c${num(o.id)}`,
      id: num(o.id),
      at: created,
      author: login(o.user),
      association: str(o.author_association),
      body: str(o.body),
      edited: Boolean(str(o.updated_at)) && str(o.updated_at) !== created,
    };
  });
}

const REVIEW_STATES = new Set(['APPROVED', 'CHANGES_REQUESTED', 'COMMENTED', 'DISMISSED', 'PENDING']);

export function parseReviews(list: unknown): Extract<TimelineItem, { type: 'review' }>[] {
  return arr(list).map((c) => {
    const o = obj(c);
    const state = str(o.state).toUpperCase();
    return {
      type: 'review' as const,
      key: `r${num(o.id)}`,
      id: num(o.id),
      at: str(o.submitted_at) || str(o.created_at),
      author: login(o.user),
      association: str(o.author_association),
      state: (REVIEW_STATES.has(state) ? state : 'COMMENTED') as ReviewState,
      body: str(o.body),
      threads: [],
    };
  });
}

function threadComment(c: unknown): ThreadComment {
  const o = obj(c);
  return {
    id: str(o.id),
    databaseId: num(o.databaseId),
    author: login(o.author),
    authorAssociation: str(o.authorAssociation),
    body: str(o.body),
    createdAt: str(o.createdAt),
    reviewId: o.pullRequestReview ? num(obj(o.pullRequestReview).databaseId) || null : null,
  };
}

export function parseThreads(conn: unknown): Thread[] {
  return nodes(conn).map((n): Thread => {
    const o = obj(n);
    const comments = nodes(o.comments).map(threadComment);
    const first = obj(nodes(o.comments)[0]);
    const line = o.line === null || o.line === undefined ? null : num(o.line) || null;
    return {
      id: str(o.id),
      path: str(o.path),
      line: line ?? (num(o.originalLine) || null),
      startLine: num(o.startLine) || null,
      side: str(o.diffSide).toUpperCase() === 'LEFT' ? 'LEFT' : 'RIGHT',
      resolved: o.isResolved === true,
      outdated: o.isOutdated === true,
      canResolve: o.viewerCanResolve === true || o.viewerCanUnresolve === true,
      canReply: o.viewerCanReply !== false,
      diffHunk: str(first.diffHunk),
      comments,
    };
  });
}

const abbrev = (oid: unknown) => str(oid).slice(0, 7);

/** Timeline events as one line each; a type not listed here is left out. */
const EVENTS: Record<string, (n: Obj) => string | null> = {
  LabeledEvent: (n) => `added the ${str(obj(n.label).name)} label`,
  UnlabeledEvent: (n) => `removed the ${str(obj(n.label).name)} label`,
  ClosedEvent: () => 'closed this',
  ReopenedEvent: () => 'reopened this',
  MergedEvent: (n) => `merged commit ${abbrev(obj(n.commit).oid)} into ${str(n.mergeRefName) || 'the base branch'}`,
  ConvertToDraftEvent: () => 'marked this pull request as draft',
  ReadyForReviewEvent: () => 'marked this pull request as ready for review',
  AssignedEvent: (n) => `assigned ${login(n.assignee)}`,
  UnassignedEvent: (n) => `unassigned ${login(n.assignee)}`,
  ReviewRequestedEvent: (n) => `requested a review from ${login(n.requestedReviewer) || 'a reviewer'}`,
  ReviewRequestRemovedEvent: (n) => `removed the review request for ${login(n.requestedReviewer) || 'a reviewer'}`,
  HeadRefForcePushedEvent: () => 'force-pushed the branch',
  HeadRefDeletedEvent: (n) => `deleted the ${str(n.headRefName)} branch`,
  HeadRefRestoredEvent: () => 'restored the head branch',
  BaseRefChangedEvent: () => 'changed the base branch',
  RenamedTitleEvent: (n) => `changed the title from "${str(n.previousTitle)}" to "${str(n.currentTitle)}"`,
  CrossReferencedEvent: (n) => {
    const s = obj(n.source);
    return `mentioned this pull request in ${str(s.title) || 'another item'} (#${num(s.number)})`;
  },
};

export function parseTimelineEvents(conn: unknown): TimelineItem[] {
  const out: TimelineItem[] = [];
  nodes(conn).forEach((raw, i) => {
    const n = obj(raw);
    const t = str(n.__typename);
    if (t === 'PullRequestCommit') {
      const c = obj(n.commit);
      out.push({
        type: 'commit',
        key: `k${str(c.oid) || i}`,
        at: str(c.committedDate) || str(c.authoredDate),
        sha: str(c.oid),
        headline: str(c.messageHeadline),
        author: login(obj(c.author).user) || str(obj(c.author).name),
      });
      return;
    }
    const text = EVENTS[t]?.(n);
    if (!text) return;
    out.push({ type: 'event', key: `e${str(n.id) || i}`, at: str(n.createdAt), actor: login(n.actor), text });
  });
  return out;
}

/** Joins the sources into one list in time order; every thread appears exactly once. */
export function buildTimeline(parts: {
  comments: TimelineItem[];
  reviews: Extract<TimelineItem, { type: 'review' }>[];
  events: TimelineItem[];
  threads: Thread[];
}): TimelineItem[] {
  const byReview = new Map<number, Thread[]>();
  const loose: Thread[] = [];
  const reviewIds = new Set(parts.reviews.map((r) => r.id));
  for (const t of parts.threads) {
    const rid = t.comments[0]?.reviewId ?? null;
    if (rid !== null && reviewIds.has(rid)) byReview.set(rid, [...(byReview.get(rid) ?? []), t]);
    else loose.push(t);
  }
  const reviews = parts.reviews.map((r) => ({ ...r, threads: byReview.get(r.id) ?? [] }));
  const threadItems: TimelineItem[] = loose.map((t) => ({ type: 'thread', key: `t${t.id}`, at: t.comments[0]?.createdAt ?? '', thread: t }));
  const all = [...parts.events, ...parts.comments, ...reviews, ...threadItems];
  return all.map((x, i) => ({ x, i })).sort((a, b) => (a.x.at < b.x.at ? -1 : a.x.at > b.x.at ? 1 : a.i - b.i)).map((e) => e.x);
}

// ---- Commits ----

export function parseCommits(conn: unknown): Commit[] {
  return nodes(conn).map((n) => {
    const c = obj(obj(n).commit);
    const sig = obj(c.signature);
    const signed = Object.keys(sig).length > 0;
    return {
      sha: str(c.oid),
      headline: str(c.messageHeadline),
      author: login(obj(c.author).user) || str(obj(c.author).name),
      date: str(c.committedDate) || str(c.authoredDate),
      verification: !signed ? 'Unsigned' : sig.isValid === true ? 'Verified' : 'Unverified',
      verificationReason: !signed ? 'This commit is not signed.' : sig.isValid === true ? 'This commit was signed with a verified signature.' : `The signature could not be verified (${str(sig.state).toLowerCase().replace(/_/g, ' ') || 'unknown reason'}).`,
    };
  });
}

// ---- Checks ----

const FAILING = new Set(['FAILURE', 'CANCELLED', 'STALE', 'TIMED_OUT', 'STARTUP_FAILURE', 'ACTION_REQUIRED']);
const SENTENCE = (s: string) => {
  const t = s.toLowerCase().replace(/_/g, ' ');
  return t.charAt(0).toUpperCase() + t.slice(1);
};

export function parseChecks(conn: unknown): CheckRow[] {
  return nodes(conn).map((raw): CheckRow => {
    const n = obj(raw);
    if (str(n.__typename) === 'StatusContext' || n.context !== undefined) {
      const st = str(n.state).toUpperCase();
      const group: CheckGroup = st === 'SUCCESS' ? 'passed' : st === 'PENDING' || st === 'EXPECTED' ? 'pending' : 'failing';
      return { name: str(n.context), app: '', description: str(n.description), group, state: SENTENCE(st || 'unknown'), durationSec: null, url: httpUrl(n.targetUrl) };
    }
    const status = str(n.status).toUpperCase();
    const conclusion = str(n.conclusion).toUpperCase();
    let group: CheckGroup;
    if (status !== 'COMPLETED') group = 'pending';
    else if (FAILING.has(conclusion)) group = 'failing';
    else if (conclusion === 'SUCCESS') group = 'passed';
    else group = 'skipped';
    const started = Date.parse(str(n.startedAt));
    const done = Date.parse(str(n.completedAt));
    const durationSec = status === 'COMPLETED' && Number.isFinite(started) && Number.isFinite(done) && done >= started ? Math.round((done - started) / 1000) : null;
    return {
      name: str(n.name),
      app: str(obj(obj(n.checkSuite).app).name),
      description: str(n.title),
      group,
      state: status !== 'COMPLETED' ? SENTENCE(status || 'queued') : SENTENCE(conclusion || 'completed'),
      durationSec,
      url: httpUrl(n.detailsUrl),
    };
  });
}

function httpUrl(raw: unknown): string | null {
  const s = str(raw);
  return /^https?:\/\//i.test(s) ? s : null;
}

const GROUP_ORDER: CheckGroup[] = ['failing', 'pending', 'passed', 'skipped'];

/** Failing, pending, passed, skipped; names sorted inside each; empty groups left out. */
export function groupChecks(rows: CheckRow[]): { group: CheckGroup; rows: CheckRow[] }[] {
  return GROUP_ORDER.map((g) => ({ group: g, rows: rows.filter((r) => r.group === g).sort((a, b) => a.name.localeCompare(b.name)) })).filter((g) => g.rows.length > 0);
}

export function formatDuration(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ${sec % 60}s`;
  return `${Math.floor(sec / 3600)}h ${Math.floor((sec % 3600) / 60)}m`;
}

// ---- Files ----

export function parseFiles(list: unknown): PrFile[] {
  return arr(list).map((raw): PrFile => {
    const o = obj(raw);
    const s = str(o.status);
    const prev = str(o.previous_filename) || null;
    const status: FileStatus = s === 'added' ? 'added' : s === 'removed' ? 'removed' : s === 'renamed' || (s === 'copied' && prev) ? 'renamed' : 'modified';
    return {
      path: str(o.filename),
      previousPath: prev,
      status,
      additions: num(o.additions),
      deletions: num(o.deletions),
      patch: typeof o.patch === 'string' ? o.patch : null,
    };
  });
}

export function parseViewed(conn: unknown): Map<string, ViewedState> {
  const m = new Map<string, ViewedState>();
  for (const n of nodes(conn)) {
    const o = obj(n);
    const v = str(o.viewerViewedState);
    m.set(str(o.path), v === 'VIEWED' || v === 'DISMISSED' ? v : 'UNVIEWED');
  }
  return m;
}
