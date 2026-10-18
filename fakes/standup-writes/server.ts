// A fake Agent Standup write server for the workflow's stage records. It serves exactly the
// routes Helm's recorder uses, behind a made-up bearer token:
//
//   POST /api/items/:id/artifacts    stores the artifact, answers 201
//   POST /api/items/:id/transition   moves the item's state, answers 200
//   GET  /api/items/:id/artifacts    lists stored artifacts (?kind= filters), bodyPreview = body
//   GET  /api/items/:id              the item and its state
//
// Any other route or method is a 404/405 and recorded as a violation. `dropNext(n)` makes the
// next n writes be stored and then have their connection destroyed before the reply, which is
// the "outcome unknown" case. `GET /__fake/state` returns everything for a check to read. No
// real board address or token appears here.
//
// CLI: `node fakes/standup-writes/server.ts [--port N] [--token T]` prints
// `fake-standup-writes listening http://127.0.0.1:<port>`. Point a debug Helm at it with
// HELM_TEST_STANDUP_URL and HELM_TEST_STANDUP_TOKEN (default `fake-writes-token`).

import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { fileURLToPath } from 'node:url';

export const DEFAULT_TOKEN = 'fake-writes-token';

export interface StoredArtifact {
  id: string;
  itemId: string;
  kind: string;
  body: string | null;
  ref: string | null;
  verdict: string | null;
  commitSha: string | null;
  createdById: string | null;
}

export interface Violation {
  method: string;
  path: string;
}

export interface FakeWrites {
  url: string;
  token: string;
  artifacts: StoredArtifact[];
  states: Map<string, string>;
  transitions: { itemId: string; to: string }[];
  violations: Violation[];
  /** Store the next `n` writes, then drop their connections before answering. */
  dropNext(n: number): void;
  close(): Promise<void>;
}

const readBody = (req: IncomingMessage): Promise<string> =>
  new Promise((resolve) => {
    let data = '';
    req.on('data', (c: Buffer) => (data += c.toString('utf8')));
    req.on('end', () => resolve(data));
  });

export async function startFakeWrites(opts: { port?: number; token?: string } = {}): Promise<FakeWrites> {
  const token = opts.token ?? DEFAULT_TOKEN;
  const artifacts: StoredArtifact[] = [];
  const states = new Map<string, string>();
  const transitions: { itemId: string; to: string }[] = [];
  const violations: Violation[] = [];
  let drops = 0;

  const send = (res: ServerResponse, status: number, body: unknown) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };

  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://fake');
    if (url.pathname === '/__fake/state') {
      return send(res, 200, { artifacts, transitions, violations, states: Object.fromEntries(states) });
    }
    if (req.headers.authorization !== `Bearer ${token}`) {
      return send(res, 401, { error: { code: 'unauthorized', message: 'Missing or wrong token.' } });
    }
    const m = /^\/api\/items\/([^/]+)(?:\/(artifacts|transition))?$/.exec(url.pathname);
    if (!m) {
      violations.push({ method: req.method ?? '', path: url.pathname });
      return send(res, 404, { error: { code: 'not_found', message: 'No such route.' } });
    }
    const itemId = decodeURIComponent(m[1]!);
    const sub = m[2];
    if (req.method === 'GET') {
      if (sub === 'artifacts') {
        const kind = url.searchParams.get('kind');
        const list = artifacts
          .filter((a) => a.itemId === itemId && (!kind || a.kind === kind))
          .map((a) => ({ ...a, bodyPreview: a.body }));
        return send(res, 200, { artifacts: list, total: list.length, nextCursor: null });
      }
      if (!sub) return send(res, 200, { id: itemId, state: states.get(itemId) ?? 'on_deck' });
    }
    if (req.method !== 'POST' || !sub) {
      violations.push({ method: req.method ?? '', path: url.pathname });
      return send(res, 405, { error: { code: 'method_not_allowed', message: 'Not a route Helm writes.' } });
    }
    let body: Record<string, unknown>;
    try {
      body = JSON.parse(await readBody(req)) as Record<string, unknown>;
    } catch {
      return send(res, 400, { error: { code: 'invalid_input', message: 'Request body must be valid JSON.' } });
    }
    const str = (k: string) => (typeof body[k] === 'string' ? (body[k] as string) : null);
    if (sub === 'artifacts') {
      if (!str('artifactKind')) return send(res, 422, { error: { code: 'invalid_input', message: 'artifactKind is required.' } });
      artifacts.push({
        id: `a${artifacts.length + 1}`,
        itemId,
        kind: str('artifactKind')!,
        body: str('body'),
        ref: str('ref'),
        verdict: str('verdict'),
        commitSha: str('commitSha'),
        createdById: str('createdById'),
      });
    } else {
      const to = str('to');
      if (!to) return send(res, 422, { error: { code: 'invalid_input', message: 'to is required.' } });
      states.set(itemId, to);
      transitions.push({ itemId, to });
    }
    if (drops > 0) {
      drops -= 1;
      req.socket.destroy();
      return;
    }
    send(res, sub === 'artifacts' ? 201 : 200, { ok: true });
  });

  await new Promise<void>((resolve) => server.listen(opts.port ?? 0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    token,
    artifacts,
    states,
    transitions,
    violations,
    dropNext: (n) => {
      drops = n;
    },
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const arg = (name: string) => {
    const i = process.argv.indexOf(name);
    return i >= 0 ? process.argv[i + 1] : undefined;
  };
  const fake = await startFakeWrites({ port: Number(arg('--port') ?? 0), token: arg('--token') });
  console.log(`fake-standup-writes listening ${fake.url}`);
}
