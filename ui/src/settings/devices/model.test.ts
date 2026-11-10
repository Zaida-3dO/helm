import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { DeviceRow } from './api.ts';
import { nameOk, otherDevices, presence } from './model.ts';

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
});
