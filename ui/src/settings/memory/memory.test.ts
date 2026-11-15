import assert from 'node:assert/strict';
import { test } from 'node:test';
import { memindexFakeOps } from '../../dev/fake-memindex.ts';
import { memindexOps } from './api.ts';
import {
  addPayload,
  docsText,
  draftProblem,
  globsProblem,
  hubNotice,
  modelProblem,
  newDraft,
  nightlyProblem,
  parseGlobs,
  pathProblem,
  pendingText,
  scanText,
  settingsChanges,
  type MemindexSource,
} from './model.ts';

const source = (p: Partial<MemindexSource> = {}): MemindexSource => ({
  id: 's1',
  kind: 'folder',
  path: 'C:\\Notes',
  personId: '',
  scope: 'me',
  include: [],
  exclude: [],
  enabled: true,
  docs: 0,
  pushed: 0,
  status: 'Waiting for the first scan.',
  lastScan: 0,
  ...p,
});

test('what the pane can refuse before asking the host, and what it says about state', () => {
  const paths: [string, RegExp | null][] = [
    ['C:\\Notes', null],
    ['C:/Notes', null],
    ['\\\\server\\share\\notes', null],
    ['D:', /full path/],
    ['Notes', /full path/],
    ['   ', /Choose a folder/],
  ];
  for (const [p, want] of paths) {
    const got = pathProblem(p);
    if (want === null) assert.equal(got, null, p);
    else assert.match(got ?? '', want, p);
  }
  const models: [string, RegExp | null][] = [
    ['nomic-embed-text', null],
    ['llama3.2:latest', null],
    ['', null],
    ['nomic embed', /letters, digits/],
    ['a".b', /letters, digits/],
    ['x'.repeat(101), /too long/],
  ];
  for (const [m, want] of models) {
    const got = modelProblem(m);
    if (want === null) assert.equal(got, null, m);
    else assert.match(got ?? '', want, m);
  }
  assert.deepEqual(['03:30', '23:59', ' 00:00 '].map((t) => nightlyProblem(t)), [null, null, null]);
  for (const t of ['3:30', '24:00', '03:60', '', 'nightly']) assert.match(nightlyProblem(t) ?? '', /HH:MM/, t);
  // globs: one per line, trimmed, blanks and repeats dropped; the host's caps mirrored.
  assert.deepEqual(parseGlobs(' Private/** \r\n\nPrivate/**\n\nDiary/**\n  '), ['Private/**', 'Diary/**']);
  assert.equal(globsProblem([]), null);
  assert.match(globsProblem(Array.from({ length: 51 }, (_, i) => `a${i}`)) ?? '', /at most 50/);
  assert.match(globsProblem(['x'.repeat(201)]) ?? '', /too long/);
  // A draft needs a kind, and folder kinds a folder; the computer's own sources need nothing else.
  assert.match(draftProblem({ kind: null, path: 'C:\\Notes' }) ?? '', /what kind/);
  assert.match(draftProblem({ kind: 'folder', path: 'Notes' }) ?? '', /full path/);
  assert.equal(draftProblem({ kind: 'chat_logs', path: '\\\\server\\logs' }), null);
  assert.equal(draftProblem({ kind: 'helm_chats', path: '' }), null);
  // The add payload: trimmed, and a computer's own source carries no folder or globs.
  const d = { ...newDraft('folder'), path: ' C:\\Notes ', exclude: ['Private/**'], include: ['*.md'] };
  assert.deepEqual(addPayload(d), { id: null, kind: 'folder', path: 'C:\\Notes', personId: null, scope: 'me', include: ['*.md'], exclude: ['Private/**'], enabled: true });
  assert.deepEqual(addPayload({ ...newDraft('claude_code') }), { id: null, kind: 'claude_code', path: null, personId: null, scope: 'me', include: null, exclude: null, enabled: true });
  assert.deepEqual(settingsChanges({ enabled: true }), { enabled: true, serverId: null, model: null, nightly: null });
  // Status lines.
  assert.deepEqual(hubNotice({ hub: 'ready' }), { text: 'The hub is taking index updates.', tone: 'ok' });
  assert.equal(hubNotice({ hub: 'too_old' }).tone, 'bad');
  assert.match(hubNotice({ hub: 'too_old' }).text, /needs updating/);
  assert.equal(hubNotice({ hub: 'down' }).tone, 'idle');
  assert.deepEqual([pendingText(null), pendingText(0)], [null, null]);
  assert.equal(pendingText(1), '1 chunk waiting to be embedded.');
  assert.equal(pendingText(1234), '1,234 chunks waiting to be embedded.');
  assert.equal(docsText(1, 0), '1 doc here, 0 on the hub');
  const now = Date.UTC(2026, 10, 15, 12);
  assert.deepEqual(
    [0, now - 30_000, now - 5 * 60_000, now - 90 * 60_000, now - 2 * 86_400_000, now - 40 * 86_400_000].map((t) => scanText(t, now)),
    ['not scanned yet', 'scanned just now', 'scanned 5 min ago', 'scanned 2 h ago', 'scanned 2 days ago', 'scanned 6 Oct'],
  );
  assert.equal(source().kind, 'folder');
});

test('the pane\'s ops against the dev host: switches, sources, refusals with plain sentences, and index now', async () => {
  const fake = memindexFakeOps();
  const events: unknown[] = [];
  const api = async <T,>(op: string, args: unknown = {}): Promise<T> => fake.handler(op, args as Record<string, unknown>, { emit: (e: unknown) => events.push(e) } as never) as T;
  const ops = memindexOps(api);

  // Opening shows the made-up sources, off, with chunks still waiting to be embedded.
  const first = await ops.state();
  assert.equal(first.enabled, false);
  assert.deepEqual(first.sources.map((s) => s.kind), ['helm_chats', 'folder']);
  assert.equal(first.pending, 12);
  assert.match(first.status, /off/);

  // Switches save whole; a bad scan time or model name is refused with a sentence.
  const on = await ops.saveSettings(settingsChanges({ enabled: true, nightly: '04:15', model: 'nomic-embed-text:latest' }));
  assert.deepEqual([on.enabled, on.nightly, on.model], [true, '04:15', 'nomic-embed-text:latest']);
  await assert.rejects(ops.saveSettings(settingsChanges({ nightly: '4:15' })), /HH:MM/);
  await assert.rejects(ops.saveSettings(settingsChanges({ model: 'two words' })), /letters, digits/);

  // A folder source: no folder, a relative folder and a repeat of one already registered are refused; a good one is added and kept.
  await assert.rejects(ops.saveSource({ id: null, kind: 'folder' }), /Choose a folder/);
  await assert.rejects(ops.saveSource({ id: null, kind: 'folder', path: 'Notes' }), /full path/);
  await assert.rejects(ops.saveSource({ id: null, kind: 'folder', path: 'C:\\Users\\dev\\Notes' }), /already a source/);
  await assert.rejects(ops.saveSource({ id: null, kind: 'helm_chats' }), /already a source/);
  const added = await ops.saveSource({ id: null, kind: 'folder', path: 'D:\\Books\\Library', personId: 'prof-sam', scope: 'everyone', exclude: ['Private/**'] });
  const lib = added.sources.find((s) => s.path === 'D:\\Books\\Library');
  assert.ok(lib && lib.personId === 'prof-sam' && lib.scope === 'everyone' && lib.exclude.join() === 'Private/**');
  // Changing one field keeps the rest; removing takes it off the list; unknown ids are refused.
  const changed = await ops.saveSource({ id: lib!.id, enabled: false });
  const off = changed.sources.find((s) => s.id === lib!.id)!;
  assert.deepEqual([off.enabled, off.path, off.scope], [false, 'D:\\Books\\Library', 'everyone']);
  const back = await ops.saveSource({ id: off.id, exclude: ['Private/**', 'Drafts/**'] });
  assert.deepEqual(back.sources.find((s) => s.id === off.id)!.exclude, ['Private/**', 'Drafts/**']);
  const removed = await ops.removeSource(off.id);
  assert.equal(removed.sources.some((s) => s.id === off.id), false);
  await assert.rejects(ops.removeSource(off.id), /not registered/);
  await assert.rejects(ops.saveSource({ id: 'ghost', enabled: false }), /not registered/);

  // Index now stamps the scans and clears the pending count; every change came in on one event kind.
  const before = (await ops.state()).sources.find((s) => s.kind === 'folder')!.lastScan;
  assert.ok(await ops.reindex({}));
  const after = (await ops.state()).sources.find((s) => s.kind === 'folder')!;
  assert.ok(after.lastScan >= before);
  assert.equal(after.lastScan > 0 || after.status !== 'Waiting for the first scan.', true);
  assert.equal((await ops.state()).pending, 0);
  assert.equal(events.every((e) => (e as { kind: string }).kind === 'memindex'), true);
  assert.ok(events.length >= 5);
});
