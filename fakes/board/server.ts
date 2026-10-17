// A fake Agent Standup read server. It serves the one
// route Helm's read-only client calls, `GET /api/projects` (paged by `limit` and `cursor`, the
// live response shape), behind a made-up bearer token. Every non-GET request is answered 405 and
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
  close(): Promise<void>;
}

export function fixtureProjects(): BoardProjectFixture[] {
  const file = fileURLToPath(new URL('./fixtures/projects.json', import.meta.url));
  return (JSON.parse(readFileSync(file, 'utf8')) as { projects: BoardProjectFixture[] }).projects;
}

function rollup(p: BoardProjectFixture) {
  // The live response carries counts and crew too; the client reads only the fields above.
  return { ...p, total: 0, merged: 0, finished: 0, counts: {}, progress: null, childless: true, assignments: [] };
}

export async function startFakeBoard(opts: { port?: number; token?: string; projects?: BoardProjectFixture[] } = {}): Promise<FakeBoard> {
  const token = opts.token ?? DEFAULT_TOKEN;
  let projects = opts.projects ?? fixtureProjects();
  const requests: LoggedRequest[] = [];
  const violations: LoggedRequest[] = [];

  const send = (res: ServerResponse, status: number, body: unknown) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };

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
    if (!authorized) return send(res, 401, { error: { code: 'unauthorized', message: 'Missing or wrong token.' } });
    if (url.pathname !== '/api/projects') return send(res, 404, { error: { code: 'not_found', message: 'No such route.' } });
    const limit = Math.min(200, Math.max(1, Number(url.searchParams.get('limit') ?? 50)));
    const cursor = url.searchParams.get('cursor');
    const start = cursor ? projects.findIndex((p) => p.id === cursor) + 1 : 0;
    const page = projects.slice(start, start + limit);
    const more = start + limit < projects.length;
    send(res, 200, { projects: page.map(rollup), childlessCount: page.length, nextCursor: more ? page[page.length - 1]!.id : null });
  });

  await new Promise<void>((resolve) => server.listen(opts.port ?? 0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    token,
    requests,
    violations,
    setProjects: (p) => {
      projects = p;
    },
    close: () => new Promise((resolve) => server.close(() => resolve())),
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
