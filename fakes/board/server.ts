// A fake Agent Standup read server. It serves the routes Helm's read-only client calls, in the
// live response shapes: projects (paged by `limit` and `cursor`), the four board columns, items
// (summary, detail, history, evidence, open loops), the fleet, readiness and search, behind a
// made-up bearer token (`/api/ready` needs none). Every non-GET request is answered 405 and
// recorded as a violation, so a test fails on any write. `GET /__fake/requests` returns the log.
//
// CLI: `node fakes/board/server.ts [--port N] [--token T]` prints
// `fake-board listening http://127.0.0.1:<port>`. Point a debug Helm at it with
// HELM_TEST_STANDUP_URL=<that url> and HELM_TEST_STANDUP_TOKEN=<the token> (default
// `fake-board-token`).

import { readFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { fileURLToPath } from 'node:url';

export const DEFAULT_TOKEN = 'fake-board-token';

export interface BoardProjectFixture {
  id: string;
  title: string;
  headline: string | null;
  area: string;
  repo: string | null;
  priority: string;
  lastActivity: string;
  /** Items under the project, and how many of them are in a terminal state. Left out, both are 0
   *  (a childless project). Equal and non-zero means the project's work is all finished. */
  total?: number;
  finished?: number;
}

export interface BoardItemFixture {
  id: string;
  projectId: string;
  title: string;
  state: string;
  priority: string;
  headline: string | null;
  checkpoint: string | null;
  body: string;
  holders: { holderId: string; holderType: string; displayName: string; role: string; machine: string }[];
  links: { key: string; url: string }[];
}

export interface LoggedRequest {
  method: string;
  path: string;
  authorized: boolean;
}

export interface FakeBoard {
  url: string;
  token: string;
  requests: LoggedRequest[];
  /** Every request that was not a GET: must stay empty. */
  violations: LoggedRequest[];
  /** Replace the projects the board serves (to test re-syncs). */
  setProjects(p: BoardProjectFixture[]): void;
  /** Replace the items the board serves. */
  setItems(i: BoardItemFixture[]): void;
  close(): Promise<void>;
}

export function fixtureProjects(): BoardProjectFixture[] {
  const file = fileURLToPath(new URL('./fixtures/projects.json', import.meta.url));
  return (JSON.parse(readFileSync(file, 'utf8')) as { projects: BoardProjectFixture[] }).projects;
}

/** The fixture items plus 32 generated backlog items, so the Backlog column needs a second page. */
export function fixtureItems(): BoardItemFixture[] {
  const file = fileURLToPath(new URL('./fixtures/items.json', import.meta.url));
  const base = (JSON.parse(readFileSync(file, 'utf8')) as { items: BoardItemFixture[] }).items;
  const filler: BoardItemFixture[] = Array.from({ length: 32 }, (_, i) => ({
    id: `b${String(i).padStart(7, '0')}-1111-4222-8333-444455556666`,
    projectId: i % 2 === 0 ? 'fb-desktop' : 'fb-website',
    title: `Backlog idea ${i + 1}`,
    state: i % 3 === 0 ? 'someday' : 'on_deck',
    priority: 'P3',
    headline: null,
    checkpoint: null,
    body: '',
    holders: [],
    links: [],
  }));
  return [...base, ...filler];
}

const COLUMN_OF: Record<string, string> = {
  someday: 'backlog',
  on_deck: 'backlog',
  planning: 'in_progress',
  plan_review: 'in_progress',
  executing: 'in_progress',
  in_review: 'in_progress',
  paused: 'waiting',
  blocked: 'waiting',
  merged: 'completed',
  research_done: 'completed',
  wont_do: 'completed',
  cancelled: 'completed',
};
const COLUMNS = ['backlog', 'in_progress', 'waiting', 'completed'];

function rollup(p: BoardProjectFixture) {
  // The live response carries per-state counts and crew too; the client reads only the fields above.
  const total = p.total ?? 0;
  const finished = p.finished ?? 0;
  return { ...p, total, merged: finished, finished, counts: {}, progress: total > 0 ? finished / total : null, childless: total === 0, assignments: [] };
}

const slimAssignment = (h: BoardItemFixture['holders'][number]) => ({
  holderId: h.holderId,
  holderType: h.holderType,
  displayName: h.displayName,
  role: h.role,
  roleCustom: null,
  liveness: 'live',
  lastActive: '2026-10-28T09:00:00.000Z',
});

const slimItem = (i: BoardItemFixture) => ({
  id: i.id,
  title: i.title,
  state: i.state,
  headline: i.headline,
  kind: 'task',
  priority: i.priority,
  area: 'desktop-app',
  repo: null,
  blockedReason: null,
  blockedOnType: null,
  blockedOnPersonId: null,
  pauseReason: null,
  originType: 'person',
  links: i.links,
});

/** A page of `list` after the entry whose key is `cursor`. */
function pageOf<T>(list: T[], limit: number, cursor: string | null, key: (x: T) => string): { part: T[]; next: string | null } {
  const start = cursor ? list.findIndex((x) => key(x) === cursor) + 1 : 0;
  const part = list.slice(start, start + limit);
  return { part, next: start + limit < list.length ? key(part[part.length - 1]!) : null };
}

export async function startFakeBoard(opts: { port?: number; token?: string; projects?: BoardProjectFixture[]; items?: BoardItemFixture[] } = {}): Promise<FakeBoard> {
  const token = opts.token ?? DEFAULT_TOKEN;
  let projects = opts.projects ?? fixtureProjects();
  let items = opts.items ?? fixtureItems();
  const requests: LoggedRequest[] = [];
  const violations: LoggedRequest[] = [];

  const send = (res: ServerResponse, status: number, body: unknown) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };
  const notFound = (res: ServerResponse) => send(res, 404, { error: { code: 'not_found', message: 'No such route or item.' } });
  const resolve = (ref: string) => items.find((i) => i.id === ref) ?? (ref.length >= 8 ? items.find((i) => i.id.startsWith(ref.toLowerCase())) : undefined);

  const history = (i: BoardItemFixture) =>
    Array.from({ length: 25 }, (_, k) => ({
      id: String(1000 - k),
      ts: new Date(Date.UTC(2026, 9, 28, 12 - Math.floor(k / 3), 0, 0)).toISOString(),
      type: k % 3 === 0 ? 'checkpoint' : 'state_changed',
      actorType: 'agent',
      actorId: 'builder-one',
      sessionId: null,
      headline: k % 3 === 0 ? `Checkpoint ${i.title} #${25 - k}` : null,
      body: k % 3 === 0 ? 'Progress noted.' : null,
      payload: null,
    }));
  const artifacts = (i: BoardItemFixture) =>
    i.holders.length === 0
      ? []
      : [{ id: 'art-1', seq: '1', kind: 'review', verdict: 'lgtm', reviewRound: 1, commitSha: null, ref: i.links[0]?.url ?? null, followUpItemId: null, createdByType: 'agent', createdById: 'reviewer-one', createdAt: '2026-10-28T10:00:00.000Z', bodyChars: 400, bodyPreview: 'Looks right to me.', bodyTruncated: true, body: 'Looks right to me. '.repeat(20), findings: null }];
  const loops = (i: BoardItemFixture) =>
    i.holders.length === 0 ? [] : [{ loopId: 'loop-1', kind: 'work', status: 'open', openedAt: '2026-10-28T09:30:00.000Z', editedAt: null, closedAt: null, text: 'Check the lock order on Windows.', textTruncated: true }];

  const server = createServer((req: IncomingMessage, res: ServerResponse) => {
    const url = new URL(req.url ?? '/', 'http://fake');
    const authorized = req.headers.authorization === `Bearer ${token}`;
    const entry = { method: req.method ?? '', path: url.pathname + url.search, authorized };
    if (url.pathname === '/__fake/requests') return send(res, 200, { requests, violations });
    requests.push(entry);
    if (req.method !== 'GET') {
      violations.push(entry);
      return send(res, 405, { error: { code: 'method_not_allowed', message: 'The fake board is read-only.' } });
    }
    const q = url.searchParams;
    const limitOf = (max: number, dflt: number) => Math.min(max, Math.max(1, Number(q.get('limit') ?? dflt)));
    if (url.pathname === '/api/ready') return send(res, 200, { ready: true, database: true, migrationsApplied: 1, migrationsPending: 0 });
    if (!authorized) return send(res, 401, { error: { code: 'unauthorized', message: 'Missing or wrong token.' } });

    if (url.pathname === '/api/projects') {
      const limit = limitOf(200, 50);
      const cursor = q.get('cursor');
      const start = cursor ? projects.findIndex((p) => p.id === cursor) + 1 : 0;
      const page = projects.slice(start, start + limit);
      const more = start + limit < projects.length;
      return send(res, 200, { projects: page.map(rollup), childlessCount: page.length, nextCursor: more ? page[page.length - 1]!.id : null });
    }

    if (url.pathname === '/api/board') {
      const column = q.get('column');
      if (!column || !COLUMNS.includes(column)) return send(res, 400, { error: { code: 'invalid_input', message: 'column is required here.', fields: ['column'] } });
      const project = q.get('project');
      const mine = items.filter((i) => COLUMN_OF[i.state] === column && (!project || i.projectId === project));
      const { part, next } = pageOf(mine, limitOf(200, 25), q.get('cursor'), (i) => i.id);
      const withheld = (c: string) => ({ entries: [], total: items.filter((i) => COLUMN_OF[i.state] === c && (!project || i.projectId === project)).length, nextCursor: null, withheld: true, revealedBy: `GET /api/board?column=${c}` });
      const columns = Object.fromEntries(
        COLUMNS.map((c) => [
          c,
          c === column
            ? { entries: part.map((i) => ({ item: slimItem(i), column: c, assignments: i.holders.map(slimAssignment), trust: null, subtasks: null })), total: mine.length, nextCursor: next, withheld: false, revealedBy: null }
            : withheld(c),
        ]),
      );
      return send(res, 200, { board: { columns, notice: null } });
    }

    if (url.pathname === '/api/fleet') {
      const rows = items.flatMap((i) =>
        i.holders.map((h) => ({ ...slimAssignment(h), id: `asg-${i.id}-${h.holderId}`, machine: h.machine, branch: 'work', worktree: null, model: null, effort: null, sessionId: 's', rootSessionId: 's', pid: null, claimedAt: '2026-10-28T08:00:00.000Z', releasedAt: null, itemId: i.id, itemTitle: i.title, itemKind: 'task', itemState: i.state })),
      );
      const { part, next } = pageOf(rows, limitOf(500, 50), q.get('cursor'), (r) => r.id);
      return send(res, 200, { assignments: part, nextCursor: next });
    }

    if (url.pathname === '/api/search') {
      const text = (q.get('q') ?? q.get('query') ?? '').trim().toLowerCase();
      if (text.length < 2) return send(res, 400, { error: { code: 'invalid_input', message: 'query must be at least 2 characters.', fields: ['query'] } });
      const found = items.filter((i) => [i.title, i.headline ?? '', i.body, ...i.links.map((l) => l.url)].some((f) => f.toLowerCase().includes(text)));
      const matches = found.slice(0, limitOf(200, 20)).map((i) => ({ id: i.id, title: i.title, state: i.state, headline: i.headline, matchedIn: 'title', score: 1, excerpt: null }));
      return send(res, 200, { matches, loopMatches: [], considered: items.length, truncated: false, notice: '' });
    }

    const m = /^\/api\/items\/([^/]+)(?:\/(detail|history|artifacts|loops)(?:\/([^/]+))?)?$/.exec(url.pathname);
    if (m) {
      const item = resolve(decodeURIComponent(m[1]!));
      if (!item) return notFound(res);
      const [, , sub, loopId] = m;
      const record = { ...slimItem(item), parentId: item.projectId, body: item.body, depth: 1 };
      if (!sub) return send(res, 200, { item: { id: item.id, title: item.title, state: item.state, headline: item.headline, checkpointHeadline: item.checkpoint } });
      if (sub === 'detail') {
        const hl = limitOf(500, 100);
        return send(res, 200, { detail: { item: record, column: COLUMN_OF[item.state], subtasks: [], artifacts: artifacts(item).slice(0, Number(q.get('artifactLimit') ?? 100)), artifactsTruncated: false, history: history(item).slice(0, Number(q.get('historyLimit') ?? hl)), historyTruncated: true, summary: null, buildStatus: null, assignments: item.holders.map(slimAssignment), previousHolders: [] } });
      }
      if (sub === 'history') {
        const all = history(item);
        const { part, next } = pageOf(all, limitOf(200, 50), q.get('cursor'), (e) => e.id);
        return send(res, 200, { entries: part, total: all.length, nextCursor: next });
      }
      if (sub === 'artifacts') {
        const all = artifacts(item);
        const { part, next } = pageOf(all, limitOf(200, 20), q.get('cursor'), (a) => a.seq);
        return send(res, 200, { artifacts: part, total: all.length, nextCursor: next });
      }
      if (loopId) {
        const l = loops(item).find((x) => x.loopId === loopId);
        return l ? send(res, 200, { ...l, itemId: item.id, text: 'Check the lock order on Windows. The full note is longer than the preview and mentions the second queue.' }) : notFound(res);
      }
      const all = loops(item);
      const { part, next } = pageOf(all, limitOf(200, 50), q.get('cursor'), (l) => l.loopId);
      return send(res, 200, { loops: part, nextCursor: next, total: all.length, nonWorkExcluded: 0 });
    }
    return notFound(res);
  });

  await new Promise<void>((resolveListen) => server.listen(opts.port ?? 0, '127.0.0.1', resolveListen));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    token,
    requests,
    violations,
    setProjects: (p) => {
      projects = p;
    },
    setItems: (i) => {
      items = i;
    },
    close: () => new Promise((resolveClose) => server.close(() => resolveClose())),
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const arg = (name: string) => {
    const i = process.argv.indexOf(name);
    return i >= 0 ? process.argv[i + 1] : undefined;
  };
  const board = await startFakeBoard({ port: Number(arg('--port') ?? 0), token: arg('--token') });
  console.log(`fake-board listening ${board.url}`);
}
