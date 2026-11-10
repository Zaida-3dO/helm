import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError, type Api } from '../api/client.ts';
import { EventBus } from '../api/events.ts';
import { chat } from '../dev/fixtures.ts';
import type { DeviceChats, DeviceInfo, DevicesList, RemoteChatRecord } from './api.ts';
import { menuLabel, menuOrder, remoteDevice, remoteList, resolveSelected, scopeNote, showToggle, stateText } from './model.ts';
import { DevicesStore, RemoteList } from './store.ts';
import { composerFor, remoteChatState, remoteHostOf, seenText, unconfirmedText, waitingForApproval } from './state.ts';

const NOW = Date.parse('2026-11-10T12:00:00');
const device = (uid: string, name: string, over: Partial<DeviceInfo> = {}): DeviceInfo => ({
  uid,
  name,
  person: null,
  online: true,
  lastSeenAt: null,
  run: true,
  isSelf: false,
  ...over,
});
const list = (...others: DeviceInfo[]): DevicesList => ({ self: 'me', devices: [device('me', 'Desk', { isSelf: true }), ...others] });

const record = (id: string, over: Partial<RemoteChatRecord> = {}): RemoteChatRecord => ({
  ...chat({ id, projectId: 'p1', cwd: 'C:/w/p1', title: id, createdAt: 1, updatedAt: 2, revision: 3, lastActivityAt: 2 }),
  hostDeviceId: 'lap',
  sharedWith: [],
  ...over,
});

test('which device shows: the stored choice, this PC when that one is gone, and how the menu reads', () => {
  const l = list(device('lap', 'Laptop'), device('abc', 'attic', { online: false, lastSeenAt: '2026-11-10T11:02:00' }));
  // The stored choice stands while that device is on the hub; otherwise (or before a list exists) it is this PC.
  assert.deepEqual([resolveSelected('lap', l), resolveSelected('gone', l), resolveSelected(null, l), resolveSelected('lap', null)], ['lap', 'me', 'me', null]);
  assert.equal(remoteDevice(l, 'lap')?.name, 'Laptop');
  assert.deepEqual([remoteDevice(l, 'me'), remoteDevice(l, null), remoteDevice(null, 'lap'), remoteDevice(l, 'gone')], [null, null, null, null]);
  // The toggle needs another device to switch to.
  assert.deepEqual([showToggle(l), showToggle(list()), showToggle(null)], [true, false, false]);
  // This PC first, the others A to Z regardless of case.
  assert.deepEqual(menuOrder(l).map((d) => d.uid), ['me', 'abc', 'lap']);
  assert.deepEqual([menuLabel(l.devices[0]!), menuLabel(l.devices[1]!)], ['Desk · this PC', 'Laptop']);
  assert.equal(stateText(l.devices[1]!, NOW), 'online');
  assert.equal(stateText({ online: false, lastSeenAt: null, isSelf: false }, NOW), 'offline');
  assert.equal(stateText({ online: false, lastSeenAt: null, isSelf: true }, NOW), 'online', 'this PC is never offline to itself');
  assert.match(stateText(l.devices[2]!, NOW), /^offline since \d/);
  assert.match(scopeNote(l.devices[1]!, NOW), /^Chats on Laptop · online$/);
});

test("another device's chats become rows and projects: groups made for projects it did not list, counts leave archived chats out", () => {
  const chats = [
    record('c1', { projectId: 'p1' }),
    record('c2', { projectId: 'p1', parentChatId: 'c1', unread: { markToken: 'm', completionId: null }, pinned: true }),
    record('c3', { projectId: 'px', projectName: 'Side', projectColour: 'blue', status: 'running', model: 'local/qwen' }),
    record('c4', { projectId: 'p1', archived: true, draft: 'half a thought' }),
  ];
  const { rows, projects } = remoteList(chats, [{ id: 'p1', name: 'Shop', colour: null, kind: 'board' }]);
  assert.deepEqual(rows.map((r) => [r.chatId, r.groupProjectId, r.hasChildren, r.attention]), [
    ['c1', 'p1', true, null],
    ['c2', 'p1', false, 'unread'],
    ['c3', 'px', false, 'running'],
    ['c4', 'p1', false, null],
  ]);
  assert.deepEqual([rows[2]!.local, rows[3]!.hasDraft, rows[3]!.archived], [true, true, true]);
  assert.deepEqual(projects.map((p) => [p.id, p.name, p.kind, p.colour, p.chatCount]), [
    ['p1', 'Shop', 'board', null, 2],
    ['px', 'Side', 'folder', 'blue', 1],
  ]);
});

/** A scripted host for the store: the devices on the hub, the stored choice, and the choices saved. */
function host(initial: { stored: string | null; devices: DevicesList | null; saveFails?: boolean }) {
  const state = { ...initial };
  const saved: unknown[] = [];
  const api = (async (op: string, args: unknown) => {
    switch (op) {
      case 'devices_list':
        if (!state.devices) throw new ApiError('unknown_op', op);
        return state.devices;
      case 'preference_get':
        return state.stored;
      case 'preference': {
        if (state.saveFails) throw new ApiError('refused', 'Not saved.');
        const a = args as { value: string | null };
        saved.push(a.value);
        state.stored = a.value;
        return null;
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  }) as Api;
  return { api, state, saved };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

test('the devices store follows the choice, falls back to this PC when that device leaves the hub, and puts the choice back if it is not saved', async () => {
  const h = host({ stored: 'lap', devices: list(device('lap', 'Laptop'), device('tab', 'Tablet')) });
  const bus = new EventBus();
  const store = new DevicesStore({ api: h.api, bus });
  store.start();
  await settle();
  assert.deepEqual([store.snapshot.selected, store.snapshot.remote?.name], ['lap', 'Laptop'], 'the saved choice survives a reload');

  // The laptop leaves the hub: this PC shows again, without anything being written.
  h.state.devices = list(device('tab', 'Tablet'));
  bus.dispatch({ kind: 'devices_changed' });
  await settle();
  assert.deepEqual([store.snapshot.selected, store.snapshot.remote], ['me', null]);
  assert.deepEqual(h.saved, []);

  // Choosing a device shows it at once and saves it.
  await store.select('tab');
  assert.deepEqual([store.snapshot.selected, h.saved], ['tab', ['tab']]);
  bus.dispatch({ kind: 'preferences', key: 'selectedDevice' });
  await settle();
  assert.equal(store.snapshot.selected, 'tab');

  // A refusal puts the old choice back and tells the caller.
  h.state.saveFails = true;
  await assert.rejects(store.select('me'), /Not saved/);
  assert.equal(store.snapshot.selected, 'tab');

  // A host with no device list (an older one): nothing is selected and the sidebar shows this PC.
  const old = new DevicesStore({ api: host({ stored: null, devices: null }).api, bus: new EventBus() });
  await old.reload();
  assert.deepEqual([old.snapshot.loaded, old.snapshot.list, old.snapshot.selected, old.snapshot.remote], [true, null, null, null]);
  store.stop();
});

test("another device's chat list pages, refreshes from the first page, and ignores an answer that a newer read overtook", async () => {
  const all = Array.from({ length: 5 }, (_, i) => record(`c${i}`));
  const asked: { offset?: number; limit?: number; archived?: boolean }[] = [];
  let gate: (() => void) | null = null;
  const ops = {
    chats: async (a: { deviceUid: string; profileId: string; archived?: boolean; offset?: number; limit?: number }): Promise<DeviceChats> => {
      asked.push({ offset: a.offset, limit: a.limit, archived: a.archived });
      if (a.profileId === 'slow') {
        await new Promise<void>((r) => (gate = r));
        return { chats: [record('late')], projects: [], more: false };
      }
      if (a.profileId === 'bad') throw new ApiError('offline', 'Laptop is offline.');
      const from = a.offset ?? 0;
      const page = all.slice(from, from + 2);
      return { chats: page, projects: [{ id: 'p1', name: 'Shop', colour: null, kind: 'folder' }], more: from + 2 < all.length };
    },
  };
  const rl = new RemoteList(ops);
  await rl.load({ deviceUid: 'lap', profileId: 'prof', archived: false });
  assert.deepEqual([rl.snapshot.rows.map((r) => r.chatId), rl.snapshot.more, rl.snapshot.loading], [['c0', 'c1'], true, false]);
  await rl.loadMore();
  assert.deepEqual(rl.snapshot.rows.map((r) => r.chatId), ['c0', 'c1', 'c2', 'c3'], 'the next page starts after the rows held');
  assert.deepEqual(asked.map((a) => a.offset), [0, 2]);
  // A refresh of the same query keeps the list until the first page answers, then holds only that page.
  const refreshing = rl.load({ deviceUid: 'lap', profileId: 'prof', archived: false });
  assert.equal(rl.snapshot.rows.length, 4);
  await refreshing;
  assert.deepEqual(rl.snapshot.rows.map((r) => r.chatId), ['c0', 'c1']);
  // Another profile starts empty, and a late answer for the first one is dropped.
  const slow = rl.load({ deviceUid: 'lap', profileId: 'slow', archived: false });
  assert.equal(rl.snapshot.rows.length, 0);
  await rl.load({ deviceUid: 'lap', profileId: 'prof', archived: true });
  assert.equal(asked.at(-1)?.archived, true);
  (gate as (() => void) | null)?.();
  await slow;
  assert.deepEqual(rl.snapshot.rows.map((r) => r.chatId), ['c0', 'c1'], 'the newer query stands');
  // A failure keeps the plain words, and clearing empties the list.
  await rl.load({ deviceUid: 'lap', profileId: 'bad', archived: false });
  assert.deepEqual([rl.snapshot.error, rl.snapshot.rows.length], ['Laptop is offline.', 0]);
  rl.clear();
  assert.deepEqual([rl.snapshot.error, rl.snapshot.more], [null, false]);
});

test('a chat on another device reads by the device state: the words, the tone, whether it can be changed', () => {
  const lap = (over: Partial<DeviceInfo> = {}) => device('lap', 'Laptop', over);
  const seen = '2026-11-10T11:02:00';
  const rows: [string, DeviceInfo | null, string, string | null, string, boolean][] = [
    ['live', lap(), 'live', null, 'neutral', true],
    ['read-only: its switch is off', lap({ run: false }), 'readonly', "Laptop doesn't let other devices run its chats. You can read this one.", 'neutral', false],
    ['offline: the hub says it is not up', lap({ online: false, lastSeenAt: null }), 'offline', "Laptop is offline (last seen never). You can read this chat; sending is off until it's back.", 'grey', false],
    ['reconnecting: the host says so, whatever the flags', lap({ state: 'reconnecting' }), 'reconnecting', 'Reconnecting to the hub… showing the copy on this PC.', 'amber', false],
    ['the host word wins over the flags', lap({ state: 'live', online: false }), 'live', null, 'neutral', true],
    ['a device that left the hub reads as offline', null, 'offline', "Another device is offline (last seen never). You can read this chat; sending is off until it's back.", 'grey', false],
  ];
  for (const [what, d, state, text, tone, live] of rows) {
    const s = remoteChatState(d, NOW);
    assert.deepEqual([s.state, s.text, s.tone, s.live], [state, text, tone, live], what);
  }
  // The last seen time reads as a time today and a date and time another day.
  const today = remoteChatState(lap({ online: false, lastSeenAt: seen }), NOW).text!;
  assert.match(today, /^Laptop is offline \(last seen \d{1,2}[:.]\d\d/);
  assert.doesNotMatch(seenText(seen, NOW), /Nov/);
  assert.ok(seenText(seen, NOW + 3 * 24 * 3600_000).length > seenText(seen, NOW).length, 'another day adds the date');
  // The composer is off with the same words, and only for a chat that runs elsewhere.
  const off = remoteChatState(lap({ run: false }), NOW);
  assert.deepEqual(composerFor('lap', off), { deviceUid: 'lap', deviceName: 'Laptop', blocked: off.text });
  assert.equal(composerFor('lap', remoteChatState(lap(), NOW)).blocked, null);
  const l = list(lap());
  assert.deepEqual([remoteHostOf({ hostDeviceId: 'lap' }, l), remoteHostOf({ hostDeviceId: 'me' }, l), remoteHostOf({ hostDeviceId: '' }, l), remoteHostOf({ hostDeviceId: 'lap' }, null), remoteHostOf(null, l)], ['lap', null, null, null, null]);
  assert.equal(unconfirmedText('Laptop'), "Laptop didn't confirm this message. It may or may not have arrived.");
  assert.equal(waitingForApproval('Laptop'), 'Waiting for approval on Laptop');
});
