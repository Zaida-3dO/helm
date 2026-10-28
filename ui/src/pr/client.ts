// The PR view's only door to the host. Reads go through two generic ops (`github_rest`, GET only,
// and `github_graphql`, persisted documents by id); writes are one typed op each. Every PR-view
// write needs a `Click` (see `clickFrom`), so loads, polls and typing can never send one.
//
// This file is also the one place that names the documents and their variables, so a change in
// the host's document list is a one-file fix.

import type { Api } from '../api/client.ts';
import { ApiError } from '../api/client.ts';
import type { LandInfo, PrRef, PrWriteResult, ViewedState } from './types.ts';

export interface RateLimit {
  remaining?: number;
  limit?: number;
  resetAt?: string;
  pausedUntil?: string;
}

export interface Answer<T = unknown> {
  status: number;
  rateLimit?: RateLimit;
  /** The next page number of a list, when there is one. */
  nextPage?: number | null;
  data: T;
  errors?: { message?: string }[];
}

// ---- Errors ----

/** A host error `[Class] sentence`, split. A plain failure (no bracket) has class `Other`. */
export class PrError extends Error {
  readonly cls: string;
  constructor(cls: string, message: string) {
    super(message);
    this.name = 'PrError';
    this.cls = cls;
  }
}

export function toPrError(e: unknown): PrError {
  if (e instanceof PrError) return e;
  const raw = e instanceof ApiError ? e.message : e instanceof Error ? e.message : typeof e === 'string' ? e : String(e);
  const m = /^\s*\[([A-Za-z]+)\]\s*(.*)$/s.exec(raw);
  if (m) return new PrError(m[1] as string, (m[2] as string) || raw);
  return new PrError('Other', raw);
}

const CLASS_TEXT: Record<string, string> = {
  Disabled: 'The pull request view is off. Turn it on under Settings, GitHub.',
  Unauthorized: 'GitHub did not accept the sign-in. Sign in again with the GitHub command line tool.',
  Unavailable: 'The GitHub command line tool (gh) is not installed or could not run.',
  ReviewPending: 'You have a review in progress on GitHub, and Helm does not add to it. Open the pull request on GitHub to finish or discard that review.',
  RateLimited: 'GitHub is limiting requests for now. Helm will try again shortly.',
  TooLarge: 'GitHub sent more than Helm will read.',
};

/** A sentence to show for an error, with the class text only where the host sent none. */
export function errorText(e: unknown): string {
  const p = toPrError(e);
  return p.message.trim() || CLASS_TEXT[p.cls] || 'Something went wrong.';
}

export function isNotFound(e: unknown): boolean {
  return /not found|\b404\b/i.test(toPrError(e).message);
}

/** Only these mean a write may have gone through; every other class means nothing was sent and the typed text stays. */
const UNCERTAIN = new Set(['Uncertain', 'Other']);

export type WriteOutcome<T> = { ok: true; value: T } | { ok: false; kind: 'refused' | 'uncertain'; cls: string; message: string };

export const UNCERTAIN_TEXT = 'Couldn’t confirm it went through. It may have been sent; refresh to check before sending again.';

/** Runs one write and classifies a failure. Anything the host did not tag as refused may have been sent. */
export async function attempt<T>(run: () => Promise<T>): Promise<WriteOutcome<T>> {
  try {
    return { ok: true, value: await run() };
  } catch (e) {
    const p = toPrError(e);
    if (UNCERTAIN.has(p.cls)) return { ok: false, kind: 'uncertain', cls: p.cls, message: UNCERTAIN_TEXT };
    // A class the host named, with its own sentence; the standing text only fills in when it sent none.
    return { ok: false, kind: 'refused', cls: p.cls, message: p.cls === 'ReviewPending' ? CLASS_TEXT.ReviewPending! : errorText(p) };
  }
}

// ---- Clicks ----

/** Proof that a write started from a person's click (or Ctrl+Enter). Minted only by `clickFrom`. */
export interface Click {
  readonly click: true;
}

const CLICK: Click = Object.freeze({ click: true });

/**
 * A click token from a trusted event. Script-made events get none in a packaged build; a dev build
 * accepts them so automated checks can drive the page.
 */
export function clickFrom(e: { isTrusted: boolean } | null | undefined): Click | null {
  if (!e) return null;
  return e.isTrusted || import.meta.env?.DEV ? CLICK : null;
}

function need(click: Click | null | undefined): void {
  if (click !== CLICK) throw new PrError('Refused', 'This needs a click.');
}

// ---- The client ----

const refVars = (r: PrRef) => ({ owner: r.owner, repo: r.repo, number: r.number });
const repoPath = (r: PrRef, tail: string) => `/repos/${r.owner}/${r.repo}/${tail}`;

export interface ReviewCommentInput {
  head: string;
  path: string;
  line: number;
  side: 'LEFT' | 'RIGHT';
  startLine?: number;
  body: string;
}

export type ReviewEvent = 'COMMENT' | 'APPROVE' | 'REQUEST_CHANGES';

export function prClient(api: Api) {
  const rest = <T = unknown>(path: string, query?: Record<string, string | number>) => api<Answer<T>>('github_rest', query ? { path, query } : { path });
  const gql = async <T = unknown>(id: string, variables: Record<string, unknown>): Promise<Answer<T>> => {
    const a = await api<Answer<T>>('github_graphql', { id, variables });
    const msgs = (a.errors ?? []).map((x) => x.message).filter(Boolean);
    // Errors next to usable data (one field GitHub could not resolve) keep the data.
    const d = a.data as unknown;
    const partial = d !== null && typeof d === 'object' && Object.values(d as object).some((v) => v !== null && v !== undefined);
    if (msgs.length > 0 && !partial) throw new PrError('GitHub', msgs.slice(0, 3).join('; '));
    return a;
  };

  return {
    // reads
    pull: (r: PrRef) => rest(repoPath(r, `pulls/${r.number}`)),
    summary: (r: PrRef) => gql('pullRequestSummary', refVars(r)),
    issueComments: (r: PrRef, page: number) => rest(repoPath(r, `issues/${r.number}/comments`), { per_page: 100, page }),
    reviews: (r: PrRef, page: number) => rest(repoPath(r, `pulls/${r.number}/reviews`), { per_page: 100, page }),
    timeline: (r: PrRef, after: string | null) => gql('pullRequestTimeline', { ...refVars(r), after }),
    threads: (r: PrRef, after: string | null) => gql('reviewThreads', { ...refVars(r), after }),
    commits: (r: PrRef, after: string | null) => gql('pullRequestCommits', { ...refVars(r), after }),
    checks: (r: PrRef, after: string | null) => gql('statusCheckRollup', { ...refVars(r), after }),
    files: (r: PrRef, page: number) => rest(repoPath(r, `pulls/${r.number}/files`), { per_page: 100, page }),
    viewed: (r: PrRef, after: string | null) => gql('pullRequestFileViewedStates', { ...refVars(r), after }),

    // writes: each needs a click and is sent once
    markViewed: async (click: Click | null, pullRequestId: string, path: string, state: ViewedState): Promise<void> => {
      need(click);
      await gql(state === 'VIEWED' ? 'markFileAsViewed' : 'unmarkFileAsViewed', { pullRequestId, path });
    },
    comment: async (click: Click | null, r: PrRef, body: string) => {
      need(click);
      return api('github_comment', { ...refVars(r), body });
    },
    reviewComment: async (click: Click | null, r: PrRef, c: ReviewCommentInput) => {
      need(click);
      return api('github_review_comment', { ...refVars(r), ...c });
    },
    reply: async (click: Click | null, r: PrRef, threadId: string, body: string) => {
      need(click);
      return api('github_review_reply', { ...refVars(r), threadId, body });
    },
    resolve: async (click: Click | null, threadId: string, resolve: boolean) => {
      need(click);
      return api('github_review_resolve', { threadId, resolve });
    },
    review: async (click: Click | null, r: PrRef, head: string, event: ReviewEvent, body: string) => {
      need(click);
      return api('github_review', { ...refVars(r), head, event, body });
    },

    // merge box and draft state: the workflow crate's ops, which obey the repository's landing rule
    landInfo: async (r: PrRef, headBranch?: string): Promise<LandInfo> => parseLandInfo(await api('pr_land_info', { ...refVars(r), ...(headBranch ? { headBranch } : {}) })),
    merge: async (click: Click | null, r: PrRef, sha: string, anyway = false): Promise<PrWriteResult> => {
      need(click);
      return parseResult(await api('pr_merge', { ...refVars(r), sha, ...(anyway ? { anyway: true } : {}) }));
    },
    ready: async (click: Click | null, r: PrRef, anyway = false): Promise<PrWriteResult> => {
      need(click);
      return parseResult(await api('pr_ready', { ...refVars(r), ...(anyway ? { anyway: true } : {}) }));
    },
    draft: async (click: Click | null, r: PrRef): Promise<PrWriteResult> => {
      need(click);
      return parseResult(await api('pr_draft', refVars(r)));
    },

    // settings and search
    viewer: () => api<Answer<{ login?: string }>>('github_viewer', {}),
    settings: () => api<{ enabled: boolean; revision: number }>('github_settings_get', {}),
    search: (query: string) => api<Answer<unknown>>('github_search_prs', { query }),
  };
}

export type PrClient = ReturnType<typeof prClient>;

function parseResult(raw: unknown): PrWriteResult {
  const o = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  return {
    outcome: typeof o.outcome === 'string' ? o.outcome : 'accepted',
    method: typeof o.method === 'string' ? o.method : undefined,
    mergedSha: typeof o.mergedSha === 'string' ? o.mergedSha : undefined,
    overridden: o.overridden === true,
    recordError: typeof o.recordError === 'string' ? o.recordError : undefined,
  };
}

/** What a PrLandInfo answer means to the merge box. */
export function parseLandInfo(raw: unknown): LandInfo {
  const o = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const t = o.tracked && typeof o.tracked === 'object' ? (o.tracked as Record<string, unknown>) : null;
  const rule = (o.rule && typeof o.rule === 'object' ? o.rule : {}) as Record<string, unknown>;
  const mode = typeof rule.mode === 'string' ? rule.mode : '';
  const routes = t?.routes === 'merge' || t?.routes === 'ready' ? t.routes : null;
  return {
    rule: mode === 'draft' ? 'draft' : mode === 'merge' ? 'merge' : null,
    method: typeof rule.method === 'string' && rule.method ? rule.method : null,
    tracked: t
      ? { title: typeof t.title === 'string' ? t.title : 'a task', state: String(t.state ?? ''), blocking: t.blocking === true, busy: t.busy === true, routes }
      : null,
  };
}

export const METHOD_LABEL: Record<string, string> = {
  squash: 'Squash and merge',
  merge: 'Create a merge commit',
  rebase: 'Rebase and merge',
  fast_forward: 'Fast-forward merge',
};
