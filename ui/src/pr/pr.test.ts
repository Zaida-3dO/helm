import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';
import { ApiError } from '../api/client.ts';
import type { FakeOpHandler } from '../dev/fakeHost.ts';
import { githubFakeOps } from '../dev/fake-github.ts';
import { parsePrSearch } from '../search/prSource.ts';
import { appendToDraft, clean, excerpt, fenceFor, fileBlock, linesBlock, prBlock } from './addToChat.ts';
import { PrError, attempt, clickFrom, parseLandInfo, prClient, toPrError } from './client.ts';
import { NO_FILTER, filterFiles, groupByDir, isTestFile, sortFiles, statBlocks, withoutTests } from './files.ts';
import { findMentions, mergeOpen, parsePrUrl, prKey, repoFromRemote } from './links.ts';
import { buildTimeline, formatDuration, groupChecks, parseChecks, parseCommits, parseFiles, parseSummary, parseThreads } from './parse.ts';
import { mentionRepoFor } from './mentionRepo.ts';
import { PrEntry } from './store.ts';
import type { PrFile } from './types.ts';
import { parseDiff } from '../panel/changes/diffModel.ts';

// ---- links and mentions ----

test('PR addresses open a tab only for github.com pull requests, with the right view and anchor', () => {
  const rows: [string, unknown][] = [
    ['https://github.com/a/b/pull/12', { owner: 'a', repo: 'b', number: 12, view: 'conversation' }],
    ['https://github.com/a/b/pull/12/', { owner: 'a', repo: 'b', number: 12, view: 'conversation' }],
    ['https://github.com/a/b/pull/12/files?w=1', { owner: 'a', repo: 'b', number: 12, view: 'files' }],
    ['https://github.com/a/b/pull/12/checks', { owner: 'a', repo: 'b', number: 12, view: 'checks' }],
    ['https://github.com/a/b/pull/12#issuecomment-99', { owner: 'a', repo: 'b', number: 12, view: 'conversation', anchor: { kind: 'issue', id: 99 } }],
    ['https://github.com/a/b/pull/12/files#discussion_r5', { owner: 'a', repo: 'b', number: 12, view: 'files', anchor: { kind: 'review', id: 5 } }],
    ['https://github.com/a/b/pull/12/checks#issuecomment-7', { owner: 'a', repo: 'b', number: 12, view: 'conversation', anchor: { kind: 'issue', id: 7 } }],
    ['https://github.com/a/b/pull/12/commits', null],
    ['https://github.com/a/b/pull/12/commits/abc', null],
    ['https://github.com/a/b/pull/0', null],
    ['https://github.com/a/b/pull/012', null],
    ['https://github.com/a/b/issues/12', null],
    ['https://github.example.com/a/b/pull/12', null],
    ['https://user@github.com/a/b/pull/12', null],
    ['https://github.com:8443/a/b/pull/12', null],
    ['http://github.com/a/b/pull/12', null],
    ['https://github.com/../b/pull/12', null],
    ['not a url', null],
  ];
  for (const [href, want] of rows) assert.deepEqual(parsePrUrl(href), want, href);
  assert.equal(prKey({ owner: 'A', repo: 'B', number: 3 }), 'a/b#3');
  // a bare link keeps the open view; one that names a view or an anchor changes it
  const open = { owner: 'a', repo: 'b', number: 1, view: 'files' as const };
  assert.equal(mergeOpen(open, { ...open, view: 'conversation' }), open);
  assert.equal(mergeOpen(open, { ...open, view: 'checks' }).view, 'checks');
  assert.equal(mergeOpen(open, { ...open, view: 'conversation', anchor: { kind: 'issue', id: 1 } }).view, 'conversation');
});

test('mentions: owner/repo#N always, bare #N only with a known repository, never inside names or paths', () => {
  const here = { owner: 'me', repo: 'app' };
  const pick = (text: string, repo: typeof here | null) => findMentions(text, repo).map((m) => `${m.ref.owner}/${m.ref.repo}#${m.ref.number}`);
  const rows: [string, typeof here | null, string[]][] = [
    ['see #12 and other/repo#7.', here, ['me/app#12', 'other/repo#7']],
    ['see #12', null, []],
    ['see other/repo#7', null, ['other/repo#7']],
    ['C#1 and a/b#1x and #0 and #012', here, []],
    ['(#5)', here, ['me/app#5']],
    ['https://x.test/page#5', here, []],
    ['&#35; and x#9', here, []],
  ];
  for (const [text, repo, want] of rows) {
    // `a/b#1x` is rejected by the trailing rule, `a/b#1` after a name by the leading rule
    assert.deepEqual(pick(text, repo), want, text);
  }
  const m = findMentions('x #12 y', here)[0]!;
  assert.equal('x #12 y'.slice(m.start, m.end), '#12');
  const remotes: [string, unknown][] = [
    ['https://github.com/o/r.git', { owner: 'o', repo: 'r' }],
    ['git@github.com:o/r.git', { owner: 'o', repo: 'r' }],
    ['ssh://git@github.com/o/r', { owner: 'o', repo: 'r' }],
    ['https://gitlab.com/o/r.git', null],
    ['https://github.com/o', null],
    [undefined as unknown as string, null],
  ];
  for (const [r, want] of remotes) assert.deepEqual(repoFromRemote(r), want, String(r));
});

// ---- reading answers ----

test('answers read into plain shapes: summary, checks, commits, files, threads and the timeline', () => {
  const s = parseSummary(
    { title: 'T', state: 'open', merged: false, draft: true, user: { login: 'u' }, head: { ref: 'h', sha: 'a1' }, base: { ref: 'b', sha: 'b1' }, comments: 2, review_comments: 3, labels: [{ name: 'x', color: 'zzz' }, { name: 'y', color: 'A1B2C3' }], requested_reviewers: [{ login: 'r' }], mergeable_state: 'dirty', merged_at: null },
    { data: { viewer: { login: 'me' }, repository: { pullRequest: { id: 'N1', reviewDecision: 'APPROVED', commits: { nodes: [{ commit: { statusCheckRollup: { state: 'FAILURE', contexts: { totalCount: 5 } } } }] } } } } },
  );
  assert.deepEqual([s.state, s.draft, s.comments, s.nodeId, s.viewer, s.reviewDecision, s.checksState, s.checksTotal, s.mergeableState], ['OPEN', true, 5, 'N1', 'me', 'APPROVED', 'FAILURE', 5, 'dirty']);
  assert.deepEqual(s.labels, [{ name: 'x', color: null }, { name: 'y', color: '#a1b2c3' }]);
  assert.equal(parseSummary({ merged: true, state: 'closed' }, {}).state, 'MERGED');
  assert.equal(parseSummary(null, null).title, '');

  const checks = parseChecks({
    nodes: [
      { __typename: 'CheckRun', name: 'b', status: 'COMPLETED', conclusion: 'SUCCESS', startedAt: '2026-01-01T00:00:00Z', completedAt: '2026-01-01T00:02:05Z' },
      { __typename: 'CheckRun', name: 'a', status: 'COMPLETED', conclusion: 'TIMED_OUT' },
      { __typename: 'CheckRun', name: 'c', status: 'IN_PROGRESS' },
      { __typename: 'CheckRun', name: 'd', status: 'COMPLETED', conclusion: 'NEUTRAL' },
      { __typename: 'StatusContext', context: 'ci', state: 'PENDING' },
      { __typename: 'StatusContext', context: 'lint', state: 'ERROR', targetUrl: 'javascript:alert(1)' },
    ],
  });
  assert.deepEqual(groupChecks(checks).map((g) => [g.group, g.rows.map((r) => r.name)]), [['failing', ['a', 'lint']], ['pending', ['c', 'ci']], ['passed', ['b']], ['skipped', ['d']]]);
  assert.equal(checks[0]!.durationSec, 125);
  assert.equal(checks.find((c) => c.name === 'lint')!.url, null);
  assert.deepEqual([formatDuration(42), formatDuration(125), formatDuration(3780)], ['42s', '2m 5s', '1h 3m']);

  const commits = parseCommits({ nodes: [{ commit: { oid: 'abc1234', messageHeadline: 'h', committedDate: '2026-01-01T00:00:00Z', author: { user: { login: 'u' } }, signature: { isValid: true } } }, { commit: { oid: 'd', author: { name: 'N' } } }] });
  assert.deepEqual(commits.map((c) => [c.author, c.verification]), [['u', 'Verified'], ['N', 'Unsigned']]);

  const files = parseFiles([{ filename: 'a', status: 'added', additions: 2 }, { filename: 'b', status: 'copied', previous_filename: 'c' }, { filename: 'd', status: 'changed', patch: '@@' }]);
  assert.deepEqual(files.map((f) => [f.status, f.previousPath, f.patch]), [['added', null, null], ['renamed', 'c', null], ['modified', null, '@@']]);

  // every thread appears exactly once: under its review when it has one, else on its own
  const threads = parseThreads({ nodes: [
    { id: 'T1', path: 'p', line: 3, diffSide: 'RIGHT', comments: { nodes: [{ id: 'c1', databaseId: 1, body: 'x', createdAt: '2026-01-01T02:00:00Z', pullRequestReview: { databaseId: 10 } }] } },
    { id: 'T2', path: 'p', line: null, originalLine: 9, isOutdated: true, comments: { nodes: [{ id: 'c2', databaseId: 2, createdAt: '2026-01-01T03:00:00Z' }] } },
  ] });
  assert.deepEqual(threads.map((t) => [t.line, t.outdated]), [[3, false], [9, true]]);
  const tl = buildTimeline({
    comments: [{ type: 'comment', key: 'c1', id: 1, at: '2026-01-01T01:00:00Z', author: 'a', association: '', body: '', edited: false }],
    reviews: [{ type: 'review', key: 'r10', id: 10, at: '2026-01-01T02:00:00Z', author: 'a', association: '', state: 'COMMENTED', body: '', threads: [] }],
    events: [{ type: 'event', key: 'e', at: '2026-01-01T00:30:00Z', actor: 'a', text: 'x' }],
    threads,
  });
  assert.deepEqual(tl.map((i) => i.type), ['event', 'comment', 'review', 'thread']);
  assert.equal((tl[2] as { threads: unknown[] }).threads.length, 1);
});

// ---- files ----

test('files: test detection, sort presets, directory groups, filters and the without-tests tally', () => {
  const f = (path: string, additions: number, deletions: number, extra: Partial<PrFile> = {}): PrFile => ({ path, previousPath: null, status: 'modified', additions, deletions, patch: null, ...extra });
  const tests: [string, boolean][] = [['src/a.test.ts', true], ['src/__tests__/a.ts', true], ['e2e/walk.ts', true], ['a_test.go', true], ['src/main/FooTest.java', true], ['src/a.ts', false], ['docs/testing.md', false], ['latest/a.ts', false]];
  for (const [p, want] of tests) assert.equal(isTestFile(p), want, p);
  const list = [f('package-lock.json', 9, 9), f('src/z.test.ts', 5, 0), f('src/types.ts', 1, 1), f('src/ui/Page.tsx', 30, 2), f('lib/util.ts', 3, 0)];
  const order = (p: Parameters<typeof sortFiles>[1]) => sortFiles(list, p).map((x) => x.path);
  assert.deepEqual(order('pr'), ['src/z.test.ts', 'src/types.ts', 'src/ui/Page.tsx', 'lib/util.ts', 'package-lock.json']);
  assert.deepEqual(order('tests-first').slice(0, 2), ['src/z.test.ts', 'src/types.ts']);
  assert.deepEqual(order('tests-last').slice(-2), ['src/z.test.ts', 'package-lock.json']);
  assert.deepEqual(order('largest').slice(0, 2), ['src/ui/Page.tsx', 'src/z.test.ts']);
  assert.deepEqual(order('smallest').slice(0, 2), ['src/types.ts', 'lib/util.ts']);
  assert.deepEqual(order('logic'), ['src/types.ts', 'lib/util.ts', 'src/ui/Page.tsx', 'src/z.test.ts', 'package-lock.json']);
  assert.deepEqual(groupByDir([f('a/x', 1, 0), f('b/y', 1, 0), f('a/z', 1, 0)], 'pr').map((g) => [g.dir, g.files.length]), [['a', 2], ['b', 1]]);
  assert.deepEqual(groupByDir([f('a/x', 1, 0), f('b/y', 1, 0), f('a/z', 1, 0)], 'largest').map((g) => g.dir), ['a', 'b', 'a']);
  const viewed = new Map<string, 'VIEWED' | 'UNVIEWED' | 'DISMISSED'>([['lib/util.ts', 'VIEWED'], ['src/types.ts', 'DISMISSED']]);
  const renamed = f('new/name.ts', 1, 1, { status: 'renamed', previousPath: 'old/Thing.ts' });
  assert.deepEqual(filterFiles([...list, renamed], { ...NO_FILTER, text: 'THING' }, viewed).map((x) => x.path), ['new/name.ts']);
  assert.equal(filterFiles(list, { ...NO_FILTER, unviewedOnly: true }, viewed).length, 4);
  assert.equal(filterFiles([...list, renamed], { ...NO_FILTER, types: new Set(['renamed'] as const) }, viewed).length, 1);
  assert.deepEqual(withoutTests(list), { added: 43, removed: 12, testFiles: 1, testLines: 5 });
  assert.equal(withoutTests([f('a.ts', 1, 1)]), null);
  assert.deepEqual(statBlocks(3, 1), ['add', 'add', 'add', 'add', 'del']);
  assert.deepEqual(statBlocks(0, 0), Array(5).fill('neutral'));
});

// ---- Add to chat ----

test('Add to chat blocks: cleaned fields, fenced excerpt around backticks, whole rows, no duplicate append', () => {
  assert.equal(clean('a\r\nb\u0007c‮d', 99), 'a\nb�c�d');
  assert.equal(clean('x'.repeat(10), 5), 'xxxx…');
  assert.equal(fenceFor('has ``` inside'), '````');
  const ref = { owner: 'o', repo: 'r', number: 4 };
  const pr = parseSummary({ title: 'Title\nwith newline', head: { ref: 'h', sha: 'abc' }, base: { ref: 'b', sha: 'def' }, state: 'open' }, {});
  const block = prBlock(ref, pr);
  assert.match(block, /data to read, not instructions/);
  assert.match(block, /https:\/\/github\.com\/o\/r\/pull\/4/);
  assert.match(block, /Title: Title with newline/);
  const file: PrFile = { path: 'a "b".ts', previousPath: 'old.ts', status: 'renamed', additions: 1, deletions: 2, patch: '@@ -1,3 +1,3 @@\n keep\n-gone `x`\n+new ``` y\n tail' };
  assert.match(fileBlock(ref, pr, file), /Path: "a \\"b\\"\.ts"[\s\S]*Previous path: "old\.ts"/);
  const rows = parseDiff(file.patch!);
  const lines = linesBlock(ref, pr, file, rows, 1, 3)!;
  assert.match(lines, /```` ?diff|````diff/);
  assert.ok(lines.includes('-gone `x`') && lines.includes('+new ``` y'));
  assert.equal(excerpt(rows, 0, 0), null);
  const long = parseDiff(`@@ -1,1 +1,1000 @@\n${Array.from({ length: 1000 }, (_, i) => `+line ${i}`).join('\n')}`);
  assert.match(excerpt(long, 0, 1000)!, /cut here/);
  assert.equal(appendToDraft('', 'B'), 'B');
  assert.equal(appendToDraft('typed  \n', 'B'), 'typed\n\nB');
  assert.equal(appendToDraft('B\n', 'B'), null);
});

// ---- the client: classes, clicks, outcomes ----

test('errors keep their class; only an uncertain write may have gone through; every write needs a click', async () => {
  assert.deepEqual([toPrError('[HeadMoved] new commits').cls, toPrError(new ApiError('conflict', '[Tracked] a task has it')).message, toPrError(new Error('boom')).cls], ['HeadMoved', 'a task has it', 'Other']);
  const run = (cls: string) => attempt(() => Promise.reject(new ApiError('conflict', `[${cls}] sentence`)));
  for (const cls of ['HeadMoved', 'Tracked', 'ReviewPending', 'Draft', 'DraftRule', 'Unavailable', 'Network', 'RateLimited']) {
    const r = await run(cls);
    assert.ok(!r.ok && r.kind === 'refused', cls);
  }
  for (const cls of ['Uncertain', 'Other']) {
    const r = await run(cls);
    assert.ok(!r.ok && r.kind === 'uncertain' && /may have been sent/.test(r.message), cls);
  }
  assert.equal(await attempt(() => Promise.resolve(5)).then((r) => r.ok && r.value), 5);
  const calls: string[] = [];
  const client = prClient(((op: string) => (calls.push(op), Promise.resolve({}))) as never);
  const ref = { owner: 'o', repo: 'r', number: 1 };
  const writes = [
    () => client.comment(null, ref, 'x'),
    () => client.reviewComment(null, ref, { head: 'h', path: 'p', line: 1, side: 'RIGHT', body: 'x' }),
    () => client.reply(null, ref, 't', 'x'),
    () => client.resolve(null, 't', true),
    () => client.review(null, ref, 'h', 'APPROVE', ''),
    () => client.markViewed(null, 'n', 'p', 'VIEWED'),
    () => client.merge(null, ref, 'h'),
    () => client.ready(null, ref),
    () => client.draft(null, ref),
  ];
  for (const w of writes) await assert.rejects(w(), (e: PrError) => e.cls === 'Refused');
  assert.deepEqual(calls, [], 'a write without a click sends nothing');
  assert.equal(clickFrom({ isTrusted: false }) !== null, Boolean(import.meta.env?.DEV));
  assert.ok(clickFrom({ isTrusted: true }));
  assert.equal(clickFrom(null), null);
  const info = parseLandInfo({ rule: { mode: 'draft', method: 'squash' }, tracked: { title: 'T', state: 'awaiting_merge', blocking: false, busy: false, routes: 'merge' } });
  assert.deepEqual([info.rule, info.method, info.tracked?.routes, parseLandInfo(null).tracked], ['draft', 'squash', 'merge', null]);
  assert.deepEqual(parsePrSearch({ items: [{ number: 3, title: 'T', state: 'OPEN', repository_url: 'https://api.github.com/repos/o/r' }, { number: 0, title: 'bad' }, { number: 4, repository: { nameWithOwner: 'a/b' } }] }).map((r) => `${r.ref.owner}/${r.ref.repo}#${r.ref.number}`), ['o/r#3', 'a/b#4']);
});

// ---- the cache entry, over the dev fake (which follows the host's op shapes) ----

function fakeApi() {
  const { handler } = githubFakeOps();
  const ctx = { emit: () => undefined, call: () => undefined };
  const calls: string[] = [];
  const api = (async (op: string, args: unknown) => {
    calls.push(op === 'github_graphql' ? `gql:${(args as { id: string }).id}` : op);
    return (handler as FakeOpHandler)(op, (args ?? {}) as Record<string, unknown>, ctx);
  }) as never;
  return { api, calls };
}

test('a PR entry polls, loads each view through the fake, pages only while shown, and writes in order', async () => {
  const { api, calls } = fakeApi();
  const e = new PrEntry({ owner: 'd', repo: 'h', number: 42 }, api);
  e.setShown(true, true, 'conversation');
  await e.poll();
  assert.equal(e.summary?.title, 'Add the usage meter to the status bar');
  assert.equal(e.summary?.nodeId, 'PR_node_42');
  assert.equal(e.summary?.viewer, 'sam');
  assert.equal(await e.pollIfStale().then(() => calls.filter((c) => c === 'github_rest').length), 1, 'a second poll within the interval is skipped');

  await e.loadConversation();
  const kinds = e.conv!.data.map((i) => i.type);
  assert.ok(kinds.includes('comment') && kinds.includes('review') && kinds.includes('commit') && kinds.includes('event'));
  const review = e.conv!.data.find((i) => i.type === 'review');
  assert.equal(review?.type === 'review' && review.threads.length, 2, 'both threads of the review sit under it');
  assert.ok(e.conv!.data.some((i) => i.type === 'thread'), 'the thread of no review stands alone');
  assert.equal(e.land?.method, 'squash');

  await e.loadCommits();
  assert.deepEqual(e.commits!.data.map((c) => c.verification), ['Verified', 'Unsigned', 'Unverified']);
  await e.loadChecks();
  assert.equal(e.checksPending(), true);
  assert.deepEqual(groupChecks(e.checks!.data).map((g) => g.group), ['failing', 'pending', 'passed', 'skipped']);

  e.setShown(true, true, 'files');
  await e.loadFiles();
  assert.equal(e.files!.data.files.length, 7);
  assert.equal(e.files!.data.viewed.get('docs/usage.md'), 'VIEWED');
  assert.equal(e.files!.data.headSha, e.summary!.headSha);

  // a hidden tab makes no further page requests and stores nothing
  const hidden = new PrEntry({ owner: 'd', repo: 'h', number: 50 }, fakeApi().api);
  hidden.setShown(true, true, 'conversation');
  await hidden.poll();
  const before = hidden.conv;
  hidden.setShown(false, false, 'conversation');
  hidden.client.timeline = (async () => ({ status: 200, data: { repository: { pullRequest: { timelineItems: { nodes: [], pageInfo: { hasNextPage: true, endCursor: 'x' } } } } } })) as never;
  await hidden.loadConversation();
  assert.equal(hidden.conv, before, 'an interrupted round stores nothing');

  // writes run one at a time, in the order given
  const order: string[] = [];
  const slow = e.enqueue(() => new Promise<void>((r) => setTimeout(() => (order.push('first'), r()), 20)));
  const fast = e.enqueue(async () => void order.push('second'));
  await Promise.all([slow, fast]);
  assert.deepEqual(order, ['first', 'second']);

  // resolve and unresolve change the thread in place, with no re-read
  const id = e.threads!.data[0]!.id;
  const n = calls.length;
  e.patchThread(id, { resolved: true });
  assert.equal(e.threads!.data[0]!.resolved, true);
  assert.equal(calls.length, n);

  // a refresh after a moved head raises the new-commits banner only when Files has a snapshot
  e.summary = { ...e.summary!, headSha: 'new' };
  assert.equal(e.files!.data.headSha === e.summary.headSha, false);
  e.close();
  assert.equal(e.closed, true);
});

test('the fake host answers merge, ready, draft and comments as the host does; a moved head is refused', async () => {
  const { api } = fakeApi();
  const c = prClient(api);
  const ref = { owner: 'd', repo: 'h', number: 46 };
  const click = clickFrom({ isTrusted: true });
  const sha = (await c.pull(ref)).data as { head: { sha: string } };
  assert.equal((await attempt(() => c.merge(click, ref, 'stale'))).ok, false);
  const tracked = await attempt(() => c.merge(click, ref, sha.head.sha));
  assert.ok(!tracked.ok && tracked.cls === 'Tracked' && tracked.kind === 'refused', 'a blocking task refuses an unforced merge');
  const forced = await attempt(() => c.merge(click, ref, sha.head.sha, true));
  assert.ok(forced.ok && forced.value.outcome === 'merged' && forced.value.overridden);
  assert.ok((await attempt(() => c.draft(click, { ...ref, number: 60 }))).ok);
  const blank = await attempt(() => c.comment(click, ref, '  '));
  assert.ok(!blank.ok && blank.kind === 'refused');
});

// ---- the chat's repository, and the click-only rule in the source ----

test('a chat belongs to the registered github.com repository holding its folder; the deepest checkout wins', () => {
  const rows = [
    { remoteUrl: 'https://github.com/o/outer.git', locations: [{ path: 'C:\\Code\\outer' }] },
    { remoteUrl: 'git@github.com:o/inner.git', locations: [{ path: 'c:/code/outer/packages/inner/' }] },
    { remoteUrl: 'https://gitlab.com/o/lab.git', locations: [{ path: 'D:/lab' }] },
  ];
  const cases: [string | null, string][] = [
    ['C:\\Code\\outer\\src', 'o/outer'],
    ['C:/Code/outer/packages/inner/src/x', 'o/inner'],
    ['C:/Code/outer-two', ''],
    ['D:/lab', ''],
    [null, ''],
  ];
  for (const [cwd, want] of cases) assert.equal(mentionRepoFor(rows, cwd), want, String(cwd));
});

test('only the client module names a PR-view write; every other module goes through it', () => {
  const dir = new URL('.', import.meta.url);
  const write = /github_(comment|review|review_comment|review_reply|review_resolve|graphql)|pr_(merge|ready|draft)|markFileAsViewed/;
  for (const name of readdirSync(dir)) {
    if (!/\.(ts|tsx)$/.test(name) || name === 'client.ts' || name === 'pr.test.ts') continue;
    const text = readFileSync(new URL(name, dir), 'utf8');
    assert.ok(!write.test(text), `${name} names a write op directly`);
  }
});
