import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULT_TOKEN, fixtureItems, fixtureProjects, startFakeBoard } from './server.ts';

const auth = { authorization: `Bearer ${DEFAULT_TOKEN}` };

test('the fake board pages GET /api/projects in the live shape behind the token', async () => {
  const board = await startFakeBoard();
  try {
    const r1 = await fetch(`${board.url}/api/projects?includeCompleted=true&limit=2`, { headers: auth });
    const p1 = (await r1.json()) as { projects: { id: string; lastActivity: string }[]; nextCursor: string | null };
    assert.equal(p1.projects.length, 2);
    assert.ok(p1.projects[0]!.lastActivity);
    const r2 = await fetch(`${board.url}/api/projects?limit=200&cursor=${p1.nextCursor}`, { headers: auth });
    const p2 = (await r2.json()) as { projects: unknown[]; nextCursor: string | null };
    assert.equal(p2.projects.length, fixtureProjects().length - 2);
    assert.equal(p2.nextCursor, null);
    assert.equal((await fetch(`${board.url}/api/projects`)).status, 401);
    assert.deepEqual(board.violations, []);
  } finally {
    await board.close();
  }
});

test('any non-GET is refused and recorded as a violation', async () => {
  const board = await startFakeBoard();
  try {
    const r = await fetch(`${board.url}/api/projects`, { method: 'POST', headers: auth, body: '{}' });
    assert.equal(r.status, 405);
    assert.equal(board.violations.length, 1);
    assert.equal(board.violations[0]!.method, 'POST');
  } finally {
    await board.close();
  }
});

test('the fake board serves the work counts of each project, so a finished one can be staged', async () => {
  const done = { id: 'fb-done', title: 'Shipped', headline: null, area: 'web', repo: null, priority: 'p3', lastActivity: '2026-10-12T08:00:00.000Z', total: 2, finished: 2 };
  const board = await startFakeBoard({ projects: [...fixtureProjects(), done] });
  try {
    const r = await fetch(`${board.url}/api/projects?includeCompleted=true&limit=200`, { headers: auth });
    const body = (await r.json()) as { projects: { id: string; total: number; finished: number }[] };
    const counts = Object.fromEntries(body.projects.map((p) => [p.id, [p.total, p.finished]]));
    assert.deepEqual(counts['fb-done'], [2, 2], 'all finished');
    assert.deepEqual(counts['fb-desktop'], [12, 7], 'work still open');
    assert.deepEqual(counts['fb-research'], [0, 0], 'no counts in the fixture: childless');
    // The default fixture has no finished project, so every one of them shows in the sidebar.
    assert.ok(fixtureProjects().every((p) => !(p.total && p.total === p.finished)));
  } finally {
    await board.close();
  }
});

test('the fake board answers every route the Tasks view reads, in the live shapes, and records only GETs', async () => {
  const board = await startFakeBoard();
  const get = async (path: string, withToken = true) => {
    const r = await fetch(`${board.url}${path}`, { headers: withToken ? auth : {} });
    return { status: r.status, body: (await r.json()) as Record<string, any> };
  };
  try {
    const open = fixtureItems().find((i) => i.state === 'executing')!;
    // The routes exactly as the host's client writes them.
    const col = await get('/api/board?column=backlog&level=exclude%3A0&limit=30');
    const backlog = col.body.board.columns;
    assert.equal(backlog.backlog.entries.length, 30, 'page one of the Backlog');
    assert.equal(backlog.backlog.total, 32);
    assert.ok(backlog.backlog.nextCursor);
    assert.equal(backlog.waiting.withheld, true, 'columns not asked for are withheld, with their totals');
    const next = await get(`/api/board?column=backlog&level=exclude%3A0&limit=30&cursor=${backlog.backlog.nextCursor}`);
    assert.equal(next.body.board.columns.backlog.entries.length, 2);
    assert.equal((await get('/api/board?column=in_progress&level=exclude%3A0&limit=30&project=fb-website')).body.board.columns.in_progress.total, 0, 'a project limits the column');
    const card = (await get('/api/board?column=in_progress&level=exclude%3A0&limit=30')).body.board.columns.in_progress.entries[0];
    assert.equal(card.item.id, open.id);
    assert.deepEqual(card.assignments.map((a: { displayName: string }) => a.displayName), ['builder-one']);

    assert.equal((await get(`/api/items/${open.id.slice(0, 8)}`)).body.item.checkpointHeadline, open.checkpoint, 'a short id resolves');
    assert.equal((await get('/api/items/nosuchid')).status, 404);
    assert.equal((await get(`/api/items/${open.id}/detail?historyLimit=1&artifactLimit=1`)).body.detail.item.body, open.body);
    const h1 = (await get(`/api/items/${open.id}/history?full=true&limit=20`)).body;
    assert.equal(h1.entries.length, 20);
    assert.equal(h1.total, 25);
    assert.equal((await get(`/api/items/${open.id}/history?full=true&limit=20&cursor=${h1.nextCursor}`)).body.entries.length, 5);
    assert.equal((await get(`/api/items/${open.id}/artifacts?full=true&limit=20`)).body.artifacts[0].kind, 'review');
    assert.equal((await get(`/api/items/${open.id}/loops?limit=20&includeClosed=false`)).body.loops[0].loopId, 'loop-1');
    assert.match((await get(`/api/items/${open.id}/loops/loop-1`)).body.text, /second queue/);
    const fleet = (await get('/api/fleet?limit=500')).body;
    assert.equal(fleet.assignments[0].machine, 'workbench');
    assert.equal(fleet.nextCursor, null);
    assert.deepEqual((await get('/api/search?q=merge%20queue&limit=10')).body.matches.map((m: { id: string }) => m.id), [open.id]);
    assert.equal((await get('/api/search?q=x&limit=10')).status, 400);
    // Readiness needs no token; everything else does.
    assert.equal((await get('/api/ready', false)).body.ready, true);
    assert.equal((await get('/api/fleet', false)).status, 401);
    assert.deepEqual(board.violations, []);
  } finally {
    await board.close();
  }
});
