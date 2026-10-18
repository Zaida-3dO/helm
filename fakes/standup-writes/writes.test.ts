import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULT_TOKEN, startFakeWrites } from './server.ts';

const auth = { authorization: `Bearer ${DEFAULT_TOKEN}`, 'content-type': 'application/json' };
const ITEM = '11111111-2222-4333-8444-555555555555';

test('artifacts and transitions are stored behind the token and read back', async () => {
  const fake = await startFakeWrites();
  try {
    const post = (path: string, body: unknown) =>
      fetch(`${fake.url}${path}`, { method: 'POST', headers: auth, body: JSON.stringify(body) });
    assert.equal((await post(`/api/items/${ITEM}/artifacts`, { artifactKind: 'plan', body: '[helm r-1] plan' })).status, 201);
    assert.equal((await post(`/api/items/${ITEM}/transition`, { to: 'executing' })).status, 200);
    const list = (await (await fetch(`${fake.url}/api/items/${ITEM}/artifacts?kind=plan`, { headers: auth })).json()) as {
      artifacts: { bodyPreview: string }[];
    };
    assert.equal(list.artifacts.length, 1);
    assert.ok(list.artifacts[0]!.bodyPreview.startsWith('[helm r-1]'));
    const item = (await (await fetch(`${fake.url}/api/items/${ITEM}`, { headers: auth })).json()) as { state: string };
    assert.equal(item.state, 'executing');
    assert.equal((await fetch(`${fake.url}/api/items/${ITEM}/artifacts`, { method: 'POST', body: '{}' })).status, 401);
    assert.equal((await post(`/api/items/${ITEM}/notes`, { body: 'x' })).status, 404);
    assert.equal(fake.violations.length, 1, 'a route Helm never writes is recorded');
  } finally {
    await fake.close();
  }
});

test('dropNext stores the write but loses the reply (outcome unknown)', async () => {
  const fake = await startFakeWrites();
  try {
    fake.dropNext(1);
    await assert.rejects(
      fetch(`${fake.url}/api/items/${ITEM}/artifacts`, {
        method: 'POST',
        headers: auth,
        body: JSON.stringify({ artifactKind: 'commit', commitSha: 'a'.repeat(40), body: '[helm r-2] c' }),
      }),
    );
    assert.equal(fake.artifacts.length, 1, 'it was stored even though the caller saw a failure');
  } finally {
    await fake.close();
  }
});
