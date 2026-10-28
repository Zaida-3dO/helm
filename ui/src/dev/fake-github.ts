// The dev fake host's GitHub ops: the pull request tab in a plain browser. It follows the host's
// op contracts (a generic read, persisted documents by id, one typed op per write, and the
// workflow crate's merge ops) over a few made-up pull requests: 42 is open with files, threads,
// commits and checks; 43 is a draft; 44 is merged; anything else behaves like 42. Writes change
// the in-memory data so a click can be seen to work. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { FakeOpHandler } from './fakeHost.ts';

type J = Record<string, unknown>;

const NOW = Date.now();
const iso = (minAgo: number) => new Date(NOW - minAgo * 60_000).toISOString();
const SHA = 'c0ffee0123456789abcdef0123456789abcdef01';
const BASE = 'b4se000123456789abcdef0123456789abcdef02';

const PATCH_METER = `@@ -1,8 +1,14 @@
 import { useEffect, useState } from 'react';
-import { Chip } from '../ui/index.ts';
+import { Chip, Tooltip } from '../ui/index.ts';
+import { formatReset } from './format.ts';

 export function Meter({ percent }: { percent: number }) {
   const [shown, setShown] = useState(percent);
   useEffect(() => setShown(percent), [percent]);
-  return <Chip>{shown}%</Chip>;
+  const label = \`\${shown}% of the plan used\`;
+  return (
+    <Tooltip text={label}>
+      <Chip>{shown}%</Chip>
+    </Tooltip>
+  );
 }
@@ -30,4 +36,6 @@ export function Meter({ percent }: { percent: number }) {
 export function resetNote(at: number) {
-  return new Date(at).toLocaleTimeString();
+  const text = formatReset(at);
+  return text;
 }`;

const PATCH_FORMAT = `@@ -0,0 +1,9 @@
+export function formatReset(at: number): string {
+  const mins = Math.max(0, Math.round((at - Date.now()) / 60000));
+  if (mins < 60) return \`resets in \${mins} min\`;
+  const hours = Math.floor(mins / 60);
+  return \`resets in \${hours} h \${mins % 60} min\`;
+}
+
+export const RESET_UNIT = 'minutes';
+`;

const PATCH_TEST = `@@ -0,0 +1,6 @@
+import assert from 'node:assert/strict';
+import { test } from 'node:test';
+import { formatReset } from './format.ts';
+
+test('formats a reset time', () => assert.match(formatReset(Date.now() + 90 * 60000), /1 h 30 min/));
+`;

const PATCH_OLD = `@@ -1,5 +0,0 @@
-// Old meter helpers, replaced by format.ts.
-export function legacyReset(at: number) {
-  return at;
-}
-`;

const PATCH_RENAME = `@@ -2,3 +2,3 @@
 ## Usage
-The meter shows the plan.
+The meter shows the plan and when it resets.
 `;

const FILES = [
  { filename: 'ui/src/usage/Meter.tsx', status: 'modified', additions: 8, deletions: 4, changes: 12, patch: PATCH_METER },
  { filename: 'ui/src/usage/format.ts', status: 'added', additions: 9, deletions: 0, changes: 9, patch: PATCH_FORMAT },
  { filename: 'ui/src/usage/format.test.ts', status: 'added', additions: 6, deletions: 0, changes: 6, patch: PATCH_TEST },
  { filename: 'ui/src/usage/legacy.ts', status: 'removed', additions: 0, deletions: 5, changes: 5, patch: PATCH_OLD },
  { filename: 'docs/usage.md', previous_filename: 'docs/meter.md', status: 'renamed', additions: 1, deletions: 1, changes: 2, patch: PATCH_RENAME },
  { filename: 'assets/meter.png', status: 'modified', additions: 0, deletions: 0, changes: 0 },
  { filename: 'package-lock.json', status: 'modified', additions: 14, deletions: 12, changes: 26, patch: '@@ -10,3 +10,3 @@\n a\n-b\n+c\n d' },
];

function user(login: string): J {
  return { login };
}

interface State {
  viewed: Map<string, string>;
  issueComments: J[];
  reviews: J[];
  threads: J[];
  draft: boolean;
  merged: boolean;
  nextId: number;
  updated: string;
}

function initial(number: number): State {
  const comments: J[] = [
    { id: 9001, user: user('dana'), author_association: 'MEMBER', body: 'Looks good so far. Can we show the reset time too?', created_at: iso(300), updated_at: iso(300) },
    { id: 9002, user: user('sam'), author_association: 'OWNER', body: 'Added in the second commit. See `formatReset`.', created_at: iso(120), updated_at: iso(100) },
  ];
  const reviews: J[] = [
    { id: 7001, node_id: 'RV7001', user: user('dana'), author_association: 'MEMBER', state: 'CHANGES_REQUESTED', body: 'A couple of small things before this lands.', submitted_at: iso(200) },
  ];
  const mk = (id: string, db: number, path: string, line: number, side: string, resolved: boolean, outdated: boolean, text: string, hunk: string): J => ({
    id,
    isResolved: resolved,
    isOutdated: outdated,
    viewerCanResolve: true,
    viewerCanReply: true,
    path,
    line,
    startLine: null,
    diffSide: side,
    comments: {
      nodes: [
        { id: `${id}-c1`, databaseId: db, body: text, createdAt: iso(190), author: user('dana'), authorAssociation: 'MEMBER', diffHunk: hunk, pullRequestReview: { id: 'RV7001', state: 'COMMENTED' } },
        ...(resolved ? [{ id: `${id}-c2`, databaseId: db + 1, body: 'Fixed.', createdAt: iso(150), author: user('sam'), authorAssociation: 'OWNER', diffHunk: hunk, pullRequestReview: null }] : []),
      ],
    },
  });
  const threads: J[] = [
    mk('T1', 8001, 'ui/src/usage/Meter.tsx', 12, 'RIGHT', false, false, 'Should this tooltip say what the percentage is of?', ' export function Meter\n+  const label = `${shown}% of the plan used`;'),
    mk('T2', 8003, 'ui/src/usage/format.ts', 4, 'RIGHT', true, false, 'Nit: name the unit.', '+  const hours = Math.floor(mins / 60);'),
    mk('T3', 8005, 'ui/src/usage/Meter.tsx', 3, 'LEFT', false, true, 'This import moved; see below.', '-import { Chip } from "../ui/index.ts";'),
  ];
  // The outdated thread belongs to no review, so the timeline shows it on its own.
  (((threads[2] as J).comments as J).nodes as J[])[0]!.pullRequestReview = null;
  return { viewed: new Map([['docs/usage.md', 'VIEWED']]), issueComments: comments, reviews, threads, draft: number === 43, merged: number === 44, nextId: 10_000, updated: iso(5) };
}

export function githubFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const states = new Map<number, State>();
  let enabled = true;
  const st = (n: number): State => {
    let s = states.get(n);
    if (!s) states.set(n, (s = initial(n)));
    return s;
  };
  const answer = (data: unknown, nextPage: number | null = null): J => ({ status: 200, rateLimit: { remaining: 4900, limit: 5000 }, nextPage, data });
  const number = (a: J): number => Number(a.number);

  const pull = (n: number): J => {
    const s = st(n);
    const merged = s.merged;
    return {
      number: n,
      title: n === 43 ? 'Draft: show the reset time in the meter' : n === 44 ? 'Fix the meter rounding' : 'Add the usage meter to the status bar',
      body: 'Shows the **plan usage** in the status bar and when it resets.\n\n- reads the cached value\n- no new requests\n\nCloses #12. See also demo/other#7.',
      state: merged ? 'closed' : 'open',
      merged,
      merged_at: merged ? iso(30) : null,
      merged_by: merged ? user('sam') : null,
      draft: s.draft,
      user: user('sam'),
      author_association: 'OWNER',
      head: { ref: 'usage-meter', sha: SHA, repo: { full_name: 'd/h' } },
      base: { ref: 'main', sha: BASE },
      commits: 3,
      comments: s.issueComments.length,
      review_comments: s.threads.length,
      changed_files: FILES.length,
      additions: 38,
      deletions: 22,
      created_at: iso(600),
      updated_at: s.updated,
      mergeable_state: s.draft ? 'draft' : 'clean',
      labels: [{ name: 'ui', color: '1d76db' }, { name: 'needs-docs', color: 'not-a-colour' }],
      requested_reviewers: [user('dana')],
      assignees: [user('sam')],
    };
  };

  const WRITES = ['github_comment', 'github_review_comment', 'github_review_reply', 'github_review', 'pr_merge', 'pr_ready', 'pr_draft'];
  const handler: FakeOpHandler = (op, a) => {
    if (WRITES.includes(op)) st(number(a)).updated = iso(0);
    switch (op) {
      case 'github_settings_get':
        return { enabled, revision: 1 };
      case 'github_settings_save':
        enabled = (a.enabled as boolean) ?? enabled;
        return { enabled, revision: 2 };
      case 'github_viewer':
        return answer({ login: 'sam', name: 'Sam', avatarUrl: '' });
      case 'github_rest': {
        const path = String(a.path);
        const q = (a.query ?? {}) as J;
        let m = /^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)$/.exec(path);
        if (m) return answer(pull(Number(m[1])));
        m = /^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/files$/.exec(path);
        if (m) return answer(Number(q.page ?? 1) > 1 ? [] : FILES);
        m = /^\/repos\/[^/]+\/[^/]+\/issues\/(\d+)\/comments$/.exec(path);
        if (m) return answer(st(Number(m[1])).issueComments);
        m = /^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/reviews$/.exec(path);
        if (m) return answer(st(Number(m[1])).reviews);
        throw new ApiError('invalid', `[Refused] The path ${path} is not allowed.`);
      }
      case 'github_graphql': {
        const n = number((a.variables ?? {}) as J);
        const s = st(n);
        const pr = (inner: J): J => ({ status: 200, rateLimit: { remaining: 4800 }, data: { repository: { pullRequest: inner } } });
        const page = (nodes: unknown[]): J => ({ nodes, pageInfo: { hasNextPage: false, endCursor: null } });
        switch (a.id) {
          case 'pullRequestSummary':
            return {
              status: 200,
              rateLimit: { remaining: 4800 },
              data: {
                repository: {
                  pullRequest: {
                    id: 'PR_node_42',
                    reviewDecision: s.merged ? 'APPROVED' : 'REVIEW_REQUIRED',
                    lastCommit: { nodes: [{ commit: { oid: SHA, statusCheckRollup: { state: 'PENDING' } } }] },
                  },
                },
              },
            };
          case 'pullRequestTimeline':
            return pr({
              timelineItems: page([
                { __typename: 'LabeledEvent', id: 'L1', createdAt: iso(590), actor: user('sam'), label: { name: 'ui' } },
                { __typename: 'PullRequestCommit', commit: { oid: SHA, messageHeadline: 'Show the plan usage in the status bar', committedDate: iso(580), author: { user: user('sam') } } },
                { __typename: 'ReviewRequestedEvent', id: 'R1', createdAt: iso(570), actor: user('sam'), requestedReviewer: user('dana') },
                { __typename: 'PullRequestCommit', commit: { oid: '5ec0nd0123456789abcdef0123456789abcdef03', messageHeadline: 'Add the reset time', committedDate: iso(110), author: { user: user('sam') } } },
                ...(s.merged ? [{ __typename: 'MergedEvent', id: 'M1', createdAt: iso(30), actor: user('sam'), commit: { oid: SHA }, mergeRefName: 'main' }] : []),
              ]),
            });
          case 'reviewThreads':
            return pr({ reviewThreads: page(s.threads) });
          case 'pullRequestCommits':
            return pr({
              commits: page([
                { commit: { oid: SHA, messageHeadline: 'Show the plan usage in the status bar', committedDate: iso(580), author: { user: user('sam') }, signature: { isValid: true, state: 'VALID' } } },
                { commit: { oid: '5ec0nd0123456789abcdef0123456789abcdef03', messageHeadline: 'Add the reset time', committedDate: iso(110), author: { user: user('sam') } } },
                { commit: { oid: '7h1rd00123456789abcdef0123456789abcdef04', messageHeadline: 'Tests for the reset text', committedDate: iso(20), author: { user: user('sam') }, signature: { isValid: false, state: 'UNKNOWN_KEY' } } },
              ]),
            });
          case 'statusCheckRollup':
            return pr({
              commits: {
                nodes: [
                  {
                    commit: {
                      statusCheckRollup: {
                        state: 'PENDING',
                        contexts: page([
                          { __typename: 'CheckRun', name: 'build', status: 'COMPLETED', conclusion: 'SUCCESS', startedAt: iso(12), completedAt: iso(10), detailsUrl: 'https://github.com/demo/helm/actions/runs/1', checkSuite: { app: { name: 'GitHub Actions' } } },
                          { __typename: 'CheckRun', name: 'unit tests', status: 'COMPLETED', conclusion: 'FAILURE', startedAt: iso(12), completedAt: iso(8), detailsUrl: 'https://github.com/demo/helm/actions/runs/2', checkSuite: { app: { name: 'GitHub Actions' } } },
                          { __typename: 'CheckRun', name: 'e2e', status: 'IN_PROGRESS', conclusion: null, startedAt: iso(3), checkSuite: { app: { name: 'GitHub Actions' } } },
                          { __typename: 'CheckRun', name: 'docs', status: 'COMPLETED', conclusion: 'SKIPPED', startedAt: iso(12), completedAt: iso(12), checkSuite: { app: { name: 'GitHub Actions' } } },
                        ]),
                      },
                    },
                  },
                ],
              },
            });
          case 'pullRequestFileViewedStates':
            return pr({ files: page(FILES.map((f) => ({ path: f.filename, viewerViewedState: s.viewed.get(f.filename) ?? 'UNVIEWED' }))) });
          case 'markFileAsViewed':
          case 'unmarkFileAsViewed': {
            const v = (a.variables ?? {}) as J;
            for (const x of states.values()) x.viewed.set(String(v.path), a.id === 'markFileAsViewed' ? 'VIEWED' : 'UNVIEWED');
            return { status: 200, data: {} };
          }
          default:
            throw new ApiError('invalid', `[Refused] The document ${String(a.id)} is not known.`);
        }
      }
      case 'github_comment': {
        const s = st(number(a));
        if (!String(a.body ?? '').trim()) throw new ApiError('invalid', '[Invalid] Write a comment first.');
        s.issueComments.push({ id: s.nextId++, user: user('sam'), author_association: 'OWNER', body: a.body, created_at: iso(0), updated_at: iso(0) });
        return { status: 201 };
      }
      case 'github_review_comment': {
        const s = st(number(a));
        if (a.head !== SHA) throw new ApiError('invalid', '[HeadMoved] The pull request has new commits; refresh to comment on the latest changes.');
        const id = s.nextId++;
        s.threads.push({
          id: `T${id}`,
          isResolved: false,
          isOutdated: false,
          viewerCanResolve: true,
          viewerCanReply: true,
          path: a.path,
          line: a.line,
          startLine: a.startLine ?? null,
          diffSide: a.side,
          comments: { nodes: [{ id: `C${id}`, databaseId: id, body: a.body, createdAt: iso(0), author: user('sam'), authorAssociation: 'OWNER', diffHunk: '', pullRequestReview: null }] },
        });
        return { status: 200 };
      }
      case 'github_review_reply': {
        const s = st(number(a));
        const t = s.threads.find((x) => x.id === a.threadId);
        if (!t) throw new ApiError('invalid', '[NotSent] That conversation is gone.');
        const id = s.nextId++;
        ((t.comments as J).nodes as J[]).push({ id: `C${id}`, databaseId: id, body: a.body, createdAt: iso(0), author: user('sam'), authorAssociation: 'OWNER', diffHunk: '', pullRequestReview: null });
        return { status: 200 };
      }
      case 'github_review_resolve': {
        for (const s of states.values()) for (const t of s.threads) if (t.id === a.threadId) t.isResolved = a.resolve === true;
        if (states.size === 0) for (const t of st(42).threads) if (t.id === a.threadId) t.isResolved = a.resolve === true;
        return { status: 200 };
      }
      case 'github_review': {
        const s = st(number(a));
        if (a.head !== SHA) throw new ApiError('invalid', '[HeadMoved] The pull request has new commits; refresh first.');
        s.reviews.push({ id: s.nextId++, user: user('sam'), author_association: 'OWNER', state: a.event === 'APPROVE' ? 'APPROVED' : a.event === 'REQUEST_CHANGES' ? 'CHANGES_REQUESTED' : 'COMMENTED', body: a.body, submitted_at: iso(0) });
        return { status: 200, pending: false };
      }
      case 'github_search_prs': {
        const q = String(a.query ?? '').toLowerCase();
        const rows = [
          { number: 42, title: 'Add the usage meter to the status bar', repo: 'demo/helm', state: 'open' },
          { number: 43, title: 'Draft: show the reset time in the meter', repo: 'demo/helm', state: 'open' },
          { number: 44, title: 'Fix the meter rounding', repo: 'demo/helm', state: 'merged' },
        ].filter((r) => r.title.toLowerCase().includes(q.trim()));
        // The host's search document: data.search.nodes.
        return answer({ search: { issueCount: rows.length, nodes: rows.map((r) => ({ __typename: 'PullRequest', number: r.number, title: r.title, state: r.state.toUpperCase(), repository: { nameWithOwner: r.repo } })) } });
      }
      case 'pr_land_info': {
        // 45: a task awaits the merge and takes it; 46: a task blocks it (needs the override).
        const n = number(a);
        const tracked =
          n === 45 || n === 46
            ? { runId: 'run1', chatId: 'c1', title: 'Usage meter task', status: 'active', repo: 'demo/helm', branch: 'usage-meter', state: n === 45 ? 'awaiting_merge' : 'reviewing', approvedTip: SHA, blocking: n === 46, busy: false, routes: n === 45 ? 'merge' : null }
            : null;
        return { repo: 'demo/helm', workflowOn: true, rule: { mode: 'merge', method: 'squash', explicit: true }, tracked };
      }
      case 'pr_merge': {
        const s = st(number(a));
        if (a.sha !== SHA) throw new ApiError('conflict', '[HeadMoved] New commits since you looked; refresh first.');
        if (s.draft) throw new ApiError('conflict', '[Draft] This pull request is a draft.');
        if (number(a) === 46 && a.anyway !== true) throw new ApiError('conflict', '[Tracked] The task “Usage meter task” is still reviewing this change. Merging anyway records it on the task.');
        s.merged = true;
        return { outcome: number(a) === 45 ? 'routed' : 'merged', method: 'squash', overridden: a.anyway === true };
      }
      case 'pr_ready': {
        st(number(a)).draft = false;
        return { outcome: 'ready', overridden: false };
      }
      case 'pr_draft': {
        st(number(a)).draft = true;
        return { outcome: 'converted', overridden: false };
      }
      default:
        throw new ApiError('not_implemented', `The fake GitHub has no ${op}.`);
    }
  };

  return { prefixes: ['github_', 'pr_land_info', 'pr_merge', 'pr_ready', 'pr_draft'], handler };
}
