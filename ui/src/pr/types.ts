// The pull request tab's data shapes: what the views draw, after the answers from GitHub have been
// read into plain objects (parse.ts). Nothing here holds a raw answer.

export type PrView = 'conversation' | 'commits' | 'checks' | 'files';

export interface PrRef {
  owner: string;
  repo: string;
  number: number;
}

/** A comment a link points at: an issue comment on the conversation, or a review comment. */
export interface PrAnchor {
  kind: 'issue' | 'review';
  id: number;
}

/** What a PR tab is: the pull request, the view it shows and the comment it was linked to. */
export interface PrTabData extends PrRef {
  view: PrView;
  anchor?: PrAnchor;
}

export type PrState = 'OPEN' | 'CLOSED' | 'MERGED';
export type ReviewDecision = 'APPROVED' | 'CHANGES_REQUESTED' | 'REVIEW_REQUIRED';
export type ChecksState = 'SUCCESS' | 'FAILURE' | 'ERROR' | 'PENDING' | 'EXPECTED';

export interface Label {
  name: string;
  /** A validated six-digit hex colour, or null. */
  color: string | null;
}

/** The header's data: one poll (the pull request and its summary). */
export interface PrSummary {
  nodeId: string | null;
  title: string;
  body: string;
  state: PrState;
  draft: boolean;
  author: string;
  authorAssociation: string;
  baseRef: string;
  headRef: string;
  baseSha: string;
  headSha: string;
  /** `owner/repo` the head branch lives in (differs from the base for a fork). */
  headRepo: string;
  commits: number;
  comments: number;
  changedFiles: number;
  additions: number;
  deletions: number;
  createdAt: string;
  updatedAt: string;
  mergedAt: string | null;
  mergedBy: string | null;
  /** GitHub's merge-state word (clean, dirty, blocked, behind, unstable, draft, unknown). */
  mergeableState: string;
  labels: Label[];
  reviewers: string[];
  assignees: string[];
  reviewDecision: ReviewDecision | null;
  checksState: ChecksState | null;
  checksTotal: number;
  viewer: string | null;
}

export interface Revision {
  headSha: string;
  baseSha: string;
  updatedAt: string;
}

export interface ThreadComment {
  id: string;
  databaseId: number;
  author: string;
  authorAssociation: string;
  body: string;
  createdAt: string;
  /** The global id of the review this comment belongs to. */
  reviewNodeId: string | null;
}

export type Side = 'LEFT' | 'RIGHT';

export interface Thread {
  id: string;
  path: string;
  line: number | null;
  startLine: number | null;
  side: Side;
  resolved: boolean;
  outdated: boolean;
  canResolve: boolean;
  canReply: boolean;
  /** The last lines of the hunk the first comment sits on. */
  diffHunk: string;
  comments: ThreadComment[];
}

export type ReviewState = 'APPROVED' | 'CHANGES_REQUESTED' | 'COMMENTED' | 'DISMISSED' | 'PENDING';

export type TimelineItem =
  | { type: 'comment'; key: string; id: number; at: string; author: string; association: string; body: string; edited: boolean }
  | {
      type: 'review';
      key: string;
      id: number;
      nodeId: string;
      at: string;
      author: string;
      association: string;
      state: ReviewState;
      body: string;
      threads: Thread[];
    }
  | { type: 'commit'; key: string; at: string; sha: string; headline: string; author: string }
  | { type: 'event'; key: string; at: string; actor: string; text: string }
  | { type: 'thread'; key: string; at: string; thread: Thread };

export interface Commit {
  sha: string;
  headline: string;
  author: string;
  /** ISO time; empty when GitHub gave none. */
  date: string;
  verification: 'Verified' | 'Unverified' | 'Unsigned';
  verificationReason: string;
}

export type CheckGroup = 'failing' | 'pending' | 'passed' | 'skipped';

export interface CheckRow {
  name: string;
  app: string;
  description: string;
  group: CheckGroup;
  /** Sentence-case state ("Succeeded", "In progress"). */
  state: string;
  durationSec: number | null;
  url: string | null;
}

export type FileStatus = 'added' | 'modified' | 'removed' | 'renamed';
export type ViewedState = 'VIEWED' | 'UNVIEWED' | 'DISMISSED';

export interface PrFile {
  path: string;
  previousPath: string | null;
  status: FileStatus;
  additions: number;
  deletions: number;
  /** GitHub's per-file patch (hunks only); null for binary or oversized files. */
  patch: string | null;
}

/** How this repository lands a pull request, and the workflow task (if any) that tracks it. */
export interface LandInfo {
  /** The repository's landing rule: a draft rule means merged by hand. */
  rule: 'draft' | 'merge' | null;
  /** The method the rule (or the default) uses: squash, merge, rebase or fast_forward. */
  method: string | null;
  tracked: {
    title: string;
    state: string;
    /** In a state that blocks a merge from the view (needs the override). */
    blocking: boolean;
    /** The task is landing this change right now. */
    busy: boolean;
    /** What a click here is passed to: the task's merge or its ready flip. */
    routes: 'merge' | 'ready' | null;
  } | null;
}

export type WriteOutcomeName = 'routed' | 'merged' | 'accepted' | 'ready' | 'alreadyReady' | 'converted' | 'alreadyDraft' | 'tracked';

/** What a merge, ready or draft op answers. */
export interface PrWriteResult {
  outcome: WriteOutcomeName | string;
  method?: string;
  mergedSha?: string;
  overridden: boolean;
  recordError?: string;
}
