import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiError, createApi } from './client.ts';

test('every call goes through the one api command with op and args', async () => {
  const calls: Array<[string, Record<string, unknown>]> = [];
  const api = createApi(async (cmd, payload) => {
    calls.push([cmd, payload]);
    return { ok: true };
  });
  assert.deepEqual(await api('app_info'), { ok: true });
  await api('preference', { key: 'sidebarWidth', value: 300 });
  assert.deepEqual(calls, [
    ['api', { op: 'app_info', args: null }],
    ['api', { op: 'preference', args: { key: 'sidebarWidth', value: 300 } }],
  ]);
});

test('host errors become ApiError with the host code', async () => {
  const api = createApi(async () => {
    throw { code: 'refused', message: 'This operation is not available here.' };
  });
  await assert.rejects(api('nope'), (e: unknown) => {
    assert.ok(e instanceof ApiError);
    assert.equal(e.code, 'refused');
    return true;
  });
});

test('transport failures are ApiError too', async () => {
  const api = createApi(async () => {
    throw new Error('ipc down');
  });
  await assert.rejects(api('x'), (e: unknown) => e instanceof ApiError && e.code === 'transport');
});
