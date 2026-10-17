import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULT_TOKEN, fixtureProjects, startFakeBoard } from './server.ts';

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
