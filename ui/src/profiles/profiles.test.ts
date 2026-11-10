import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError, type Api } from '../api/client.ts';
import { EventBus } from '../api/events.ts';
import type { PeerPerson, ProfilesView, Sharing } from '../api/profiles.ts';
import { profileChoices } from './menu.ts';
import { chipText, householdTargets, initialOf, lastSeenMs, offlineText, orderPeerChats, planText, profileOfChat, rowsForProfile } from './model.ts';
import { ProfilesStore } from './store.ts';

const NOW = Date.parse('2026-11-10T12:00:00');
const sum = (id: string, name: string, over: Partial<ProfilesView['profiles'][number]> = {}): ProfilesView['profiles'][number] => ({
  id,
  name,
  colour: '#6d8cff',
  hubPersonId: null,
  isHome: false,
  chats: 0,
  running: 0,
  attention: 0,
  ...over,
});
const view = (activeId: string | null, extra: Partial<ProfilesView> = {}): ProfilesView => ({
  profiles: [sum('h', 'Home', { isHome: true, hubPersonId: 'p-self' }), sum('o', 'Other', { hubPersonId: 'p-other' })],
  homeId: 'h',
  activeId,
  ...extra,
});
const all: Sharing = { chats: true, transcripts: true, messages: false };
const people: PeerPerson[] = [
  { id: 'p-self', name: 'Home', you: true, machines: [{ id: 'm1', name: 'Desk', online: true, lastSeen: NOW, sharing: all }] },
  {
    id: 'p-other',
    name: 'Other',
    you: false,
    machines: [
      { id: 'm2', name: 'Laptop', online: true, lastSeen: NOW, sharing: all },
      { id: 'm3', name: 'Tablet', online: false, lastSeen: NOW - 3_600_000, sharing: all },
    ],
  },
  { id: 'p-third', name: 'Third', you: false, machines: [] },
];

test('which profile a chat belongs to, and what each choice of profile shows', () => {
  const rows = [
    { chatId: 'a', parentChatId: null }, // unlabelled: home
    { chatId: 'b', parentChatId: null }, // labelled: other
    { chatId: 'c', parentChatId: 'b' }, // started from b: follows it
    { chatId: 'd', parentChatId: 'c' }, // two levels down: still follows
    { chatId: 'e', parentChatId: 'b' }, // started from b but labelled home-ward by an explicit label
    { chatId: 'x', parentChatId: 'y' }, // cycle
    { chatId: 'y', parentChatId: 'x' },
  ];
  const labels = new Map([['b', 'o'], ['e', 'h']]);
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  const of = (id: string) => profileOfChat(id, byId, labels, 'h');
  assert.deepEqual(['a', 'b', 'c', 'd', 'e', 'x'].map(of), ['h', 'o', 'o', 'o', 'h', 'h']);
  assert.equal(profileOfChat('a', byId, labels, null), null, 'no profile exists yet');
  const ids = (active: string | null) => rowsForProfile(rows, active, 'h', labels).map((r) => r.chatId).join('');
  assert.equal(ids(null), 'abcdexy', 'All profiles shows everything');
  assert.equal(ids('h'), 'aexy');
  assert.equal(ids('o'), 'bcd');
  assert.equal(rowsForProfile(rows, 'o', null, labels).length, rows.length, 'with no home profile nothing is hidden');
});

test('the chip, the plan, the disc and the switcher words', () => {
  assert.equal(chipText(view(null), 'Max'), 'All profiles');
  assert.equal(chipText(view('h'), 'Max'), 'Home · Max', 'the plan belongs to the home profile');
  assert.equal(chipText(view('h'), null), 'Home');
  assert.equal(chipText(view('o'), 'Max'), 'Other', 'not the home profile: no plan');
  assert.equal(chipText(view('gone'), 'Max'), 'All profiles');
  assert.equal(chipText({ profiles: [], homeId: null, activeId: null }, null), 'Who uses this Helm?');
  assert.deepEqual([planText('max'), planText(' ultra '), planText(''), planText(undefined)], ['Max', 'Ultra', null, null]);
  assert.deepEqual([initialOf('sam'), initialOf('  éa'), initialOf('')], ['S', 'É', '?']);
  assert.equal(offlineText(people[1]!.machines[0]!, NOW), 'online');
  assert.match(offlineText(people[1]!.machines[1]!, NOW), /^offline since \d/);
  assert.equal(offlineText({ ...people[1]!.machines[1]!, lastSeen: null }, NOW), 'offline');
  assert.deepEqual([lastSeenMs('2026-11-10T11:00:00.000Z'), lastSeenMs(5), lastSeenMs('never'), lastSeenMs(''), lastSeenMs(null)], [Date.parse('2026-11-10T11:00:00Z'), 5, null, null, null]);
  assert.match(offlineText({ ...people[1]!.machines[1]!, lastSeen: '2026-11-10T11:00:00.000Z' }, NOW), /^offline since /);
  assert.deepEqual(
    orderPeerChats([
      { chatId: '1', title: '', status: '', updatedAt: 1 },
      { chatId: '2', title: '', status: '', updatedAt: 5 },
      { chatId: '3', title: '', status: '', updatedAt: 2, needsAttention: true },
    ]).map((c) => c.chatId),
    ['3', '2', '1'],
  );
});

test('the household rows: every other person for All profiles, only the linked person for a profile', () => {
  const keys = (v: ProfilesView | null) => householdTargets(v, people).map((t) => t.title);
  assert.deepEqual(keys(view(null)), ['Other · Laptop', 'Other · Tablet'], 'your own person is never listed, nor a person with no computer');
  assert.deepEqual(keys(view('o')), ['Other · Laptop', 'Other · Tablet']);
  assert.deepEqual(keys(view('h')), [], 'the home profile has no household rows');
  assert.deepEqual(keys(view('gone')), []);
  assert.deepEqual(keys(null), []);
  assert.deepEqual(householdTargets(view(null), null), []);
});

test('Move to profile offers the profiles with the current one marked, and nothing when there is only one', () => {
  const snap = { loaded: true, view: view(null), labels: new Map([['b', 'o']]), sharing: all, people: null, groups: new Map(), error: null };
  const rows = [{ chatId: 'a', parentChatId: null }, { chatId: 'b', parentChatId: null }, { chatId: 'c', parentChatId: 'b' }];
  const cur = (id: string) => profileChoices(snap, id, rows).filter((c) => c.current).map((c) => c.id);
  assert.deepEqual([cur('a'), cur('b'), cur('c')], [['h'], ['o'], ['o']]);
  assert.deepEqual(profileChoices(snap, 'a', rows).map((c) => c.id), ['h', 'o'], 'home first');
  assert.deepEqual(profileChoices({ ...snap, view: { ...view(null), profiles: [sum('h', 'Home', { isHome: true })] } }, 'a', rows), []);
});

/** A scripted host: the ops the store calls, recording what it was asked. */
function host(opts: { online?: boolean; listFails?: boolean } = {}) {
  const calls: [string, unknown][] = [];
  let active: string | null = null;
  const api = (async (op: string, args: unknown) => {
    calls.push([op, args]);
    switch (op) {
      case 'profiles_list':
        return view(active);
      case 'profiles_chat_ids':
        return { chatIds: (args as { profileId: string }).profileId === 'o' ? ['b'] : [] };
      case 'profiles_assign_chats':
        return view(active);
      case 'household_get':
        return all;
      case 'relay_connection_get':
        return { hasAddress: true, hasFallback: false, hasToken: true, state: opts.online === false ? 'offline' : 'online' };
      case 'peer_people':
        return { people };
      case 'peer_list_chats': {
        if (opts.listFails) throw new ApiError('unavailable', 'Other does not share their chat list.');
        return { chats: [{ chatId: 'r1', title: 'Theirs', status: 'idle', updatedAt: 1 }] };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  }) as Api;
  return { api, calls, setActive: (id: string | null) => (active = id) };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

test('the store reads labels, labels a new chat while another person is showing, and follows the host', async () => {
  const h = host();
  h.setActive('o');
  const bus = new EventBus();
  const store = new ProfilesStore({ api: h.api, bus });
  store.start();
  await settle();
  assert.equal(store.snapshot.view?.activeId, 'o');
  assert.deepEqual([...store.snapshot.labels], [['b', 'o']], 'labels come from the profiles other than home');
  assert.deepEqual(store.snapshot.sharing, all);

  const fresh = (id: string, over: Record<string, unknown> = {}) => ({ id, parentChatId: null, sideSourceId: null, createdAt: Date.now(), archived: false, ...over });
  for (const c of [fresh('n1'), fresh('n1'), fresh('old', { createdAt: 1 }), fresh('kid', { parentChatId: 'b' }), fresh('side', { sideSourceId: 'b' })]) bus.dispatch({ kind: 'chat', chat: c });
  await settle();
  assert.equal(store.snapshot.labels.get('n1'), 'o', 'a chat started now joins the profile showing');
  for (const id of ['old', 'kid', 'side']) assert.equal(store.snapshot.labels.has(id), false, `${id} is not a new top-level chat`);
  assert.equal(h.calls.filter(([op]) => op === 'profiles_assign_chats').length, 1, 'once, however often the chat reports');

  const before = h.calls.filter(([op]) => op === 'profiles_list').length;
  bus.dispatch({ kind: 'profiles_changed' });
  await settle();
  assert.equal(h.calls.filter(([op]) => op === 'profiles_list').length, before + 1);

  // With the home profile (or All profiles) showing, a new chat is left alone: no label is the home label.
  h.setActive(null);
  bus.dispatch({ kind: 'profiles_changed' });
  await settle();
  bus.dispatch({ kind: 'chat', chat: fresh('n2') });
  await settle();
  assert.equal(store.snapshot.labels.has('n2'), false);
  store.stop();
});

test('the household is read only while watched, drops a list that stopped being shared, and clears when the hub is not connected', async () => {
  const bus = new EventBus();
  let tick: (() => void) | undefined;
  let cleared = 0;
  const deps = (api: Api) => ({ api, bus, setInterval: (f: () => void) => ((tick = f), 1), clearInterval: () => void cleared++ });

  const ok = host();
  const store = new ProfilesStore(deps(ok.api));
  assert.equal(ok.calls.length, 0, 'nothing is asked before something watches');
  const stopA = store.watchHousehold();
  const stopB = store.watchHousehold();
  await settle();
  const groups = [...store.snapshot.groups.values()];
  assert.deepEqual(groups.map((g) => [g.key, g.online, g.chats.length, g.note]), [['p-other/m2', true, 1, null], ['p-other/m3', false, 0, null]], 'the offline computer is not asked');
  assert.equal(ok.calls.filter(([op]) => op === 'peer_list_chats').length, 1);
  stopA();
  assert.equal(cleared, 0, 'one watcher is still there');
  stopB();
  stopB();
  assert.equal(cleared, 1, 'the poll stops with the last watcher, once');
  assert.ok(tick);

  const gone = new ProfilesStore(deps(host({ listFails: true }).api));
  await gone.refreshHousehold();
  assert.deepEqual([...gone.snapshot.groups.values()].map((g) => [g.chats.length, g.note]), [[0, 'Other does not share their chat list.'], [0, null]]);

  const off = new ProfilesStore(deps(host({ online: false }).api));
  await off.refreshHousehold();
  assert.equal(off.snapshot.people, null);
  assert.equal(off.snapshot.groups.size, 0);
});
