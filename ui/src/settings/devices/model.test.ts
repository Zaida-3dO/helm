import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Forward } from '../../devices/api.ts';
import { forwardAddress } from '../../devices/api.ts';
import type { DeviceRow } from './api.ts';
import { forwardRows, nameOk, otherDevices, peopleLine, presence } from './model.ts';

const NOW = Date.parse('2026-11-10T12:00:00Z');
const ago = (ms: number) => new Date(NOW - ms).toISOString();
const MIN = 60_000;

test('devices: presence in plain words, the name rule and the order of the list', () => {
  // [online, lastSeenAt, expected]
  for (const [online, lastSeenAt, want] of [
    [true, null, 'online'],
    [false, null, 'never connected'],
    [false, 'not a time', 'never connected'],
    [false, ago(20_000), 'last seen just now'],
    [false, ago(MIN), 'last seen 1 minute ago'],
    [false, ago(95 * MIN), 'last seen 1 hour ago'],
    [false, ago(5 * 60 * MIN), 'last seen 5 hours ago'],
    [false, ago(49 * 60 * MIN), 'last seen 2 days ago'],
  ] as const) {
    assert.equal(presence({ online, lastSeenAt }, NOW), want, `${online}/${lastSeenAt}`);
  }

  for (const [name, ok] of [['Desk', true], ['  ', false], ['', false], ['x'.repeat(40), true], ['x'.repeat(41), false]] as const) {
    assert.equal(nameOk(name), ok, name);
  }

  const row = (uid: string, name: string, online: boolean, isSelf = false): DeviceRow => ({ uid, name, person: null, online, lastSeenAt: null, run: true, isSelf });
  const list = [row('s', 'Studio', false), row('me', 'Desk', true, true), row('l', 'Laptop', true), row('a', 'Attic', false)];
  assert.deepEqual(otherDevices(list).map((d) => d.uid), ['l', 'a', 's']);

  // The people line names everyone a computer acts for; an older report without the list falls back.
  const ppl: [Pick<DeviceRow, 'person' | 'people'>, string][] = [
    [{ person: 'Pat', people: [{ personId: 'p', person: 'Pat' }, { personId: 'q', person: 'Sam' }] }, 'Pat, Sam'],
    [{ person: 'Pat', people: [] }, 'Pat'],
    [{ person: 'Pat' }, 'Pat'],
    [{ person: null, people: [{ personId: 'p', person: '' }] }, 'Unknown person'],
    [{ person: null }, 'Unknown person'],
  ];
  for (const [d, want] of ppl) assert.equal(peopleLine(d), want);
});

test('forwards: one row per port in plain words, by PC then port, and only loopback addresses open', () => {
  const fw = (over: Partial<Forward>): Forward => ({
    forwardId: 'f', chatId: 'c1', deviceUid: 'd', deviceName: 'Desk', port: 5173, localPort: 5173, url: 'http://127.0.0.1:5173/',
    title: '', streams: 0, openedAt: NOW - 10 * MIN, lastUsedAt: NOW - 5 * MIN, ...over,
  });
  const list = [
    fw({ forwardId: 'b', port: 8080, localPort: 8081, title: 'API', streams: 2 }),
    fw({ forwardId: 'a' }),
    fw({ forwardId: 'c', chatId: 'c2', deviceName: 'Attic', port: 3000, localPort: 3000, streams: 1, lastUsedAt: NOW }),
    fw({ forwardId: 'd', chatId: 'c2', deviceName: 'Attic', port: 4000, localPort: 4000, openedAt: NOW - 20_000, lastUsedAt: 0 }),
  ];
  assert.deepEqual(
    forwardRows(list, NOW).map((r) => [r.forward.forwardId, r.line, r.use]),
    [
      ['c', 'Port 3000 on Attic opens here at localhost:3000', '1 connection open'],
      ['d', 'Port 4000 on Attic opens here at localhost:4000', 'idle'],
      ['a', 'Port 5173 on Desk opens here at localhost:5173', 'idle for 5 minutes'],
      ['b', 'Port 8080 on Desk (API) opens here at localhost:8081 (8080 was taken here)', '2 connections open'],
    ],
  );
  assert.deepEqual(forwardRows(list, NOW, 'c1').map((r) => r.forward.forwardId), ['a', 'b']);
  assert.deepEqual(forwardRows([], NOW), []);
  // The browser gets an address built here, never the host's words as written.
  assert.deepEqual([5173, 0, 70000, 1.5].map((localPort) => forwardAddress({ localPort })), ['http://localhost:5173/', null, null, null]);
});
