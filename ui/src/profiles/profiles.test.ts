import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ApiError, type Api } from '../api/client.ts';
import { EventBus } from '../api/events.ts';
import type { HubPerson, ProfilesView } from '../api/profiles.ts';
import { profileChoices, shareChoices } from './menu.ts';
import { belongsTo, initialOf, itemText, lastSeenMs, needsFirstRun, rowsForProfile, sharedFrom, sharedWithOf, stepTile, tileCounts } from './model.ts';
import { ProfilesStore } from './store.ts';

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
const people: HubPerson[] = [
  { id: 'p-self', name: 'Home', you: true },
  { id: 'p-other', name: 'Other', you: false },
];

test('which chats a profile shows: its own and those shared with it, a started chat following its source', () => {
  const rows = [
    { chatId: 'a', parentChatId: null }, // in the list
    { chatId: 'b', parentChatId: null }, // not in the list
    { chatId: 'c', parentChatId: 'a' }, // started from a: follows it
    { chatId: 'd', parentChatId: 'c' }, // two levels down: still follows
    { chatId: 'e', parentChatId: 'b' }, // started from a chat that is not in the list
    { chatId: 'x', parentChatId: 'y' }, // cycle
    { chatId: 'y', parentChatId: 'x' },
    { chatId: 'f', parentChatId: null, profileId: 'o', sharedWith: ['h'] }, // the host says it: shared with h
    { chatId: 'g', parentChatId: null, profileId: 'o', sharedWith: [] }, // the host says it: o's, not shared
    { chatId: 'i', parentChatId: 'a', profileId: 'o' }, // the host's word beats the chat it came from
  ];
  const ids = new Set(['a']);
  const shown = (active: string | null, set: ReadonlySet<string> | null) => rowsForProfile(rows, active, set).map((r) => r.chatId).join('');
  assert.equal(shown('h', ids), 'acdf', 'own and shared, plus what follows a listed chat');
  assert.equal(shown(null, ids), 'abcdexyfgi', 'no active profile shows everything');
  assert.equal(shown('h', null), 'abcdexyf', 'before the list is read, rows the host names are judged and the rest show');
  assert.equal(shown('o', new Set(['b'])), 'befgi', 'o: its own rows by the host, b and its child by the list');
  assert.deepEqual(
    [sharedFrom(rows[7]!, 'h'), sharedFrom(rows[8]!, 'h'), sharedFrom(rows[7]!, 'o'), sharedFrom(rows[0]!, 'h'), sharedFrom(rows[7]!, null)],
    ['o', null, null, null, null],
    'the owner is named only for a chat shared with the active profile',
  );
  assert.deepEqual([belongsTo({ profileId: 'o', sharedWith: ['h'] }, 'h'), belongsTo({ profileId: 'o' }, 'h'), belongsTo({}, 'h')], [true, false, null]);
  assert.deepEqual([...sharedWithOf({ chatId: 'f', parentChatId: null, sharedWith: ['h'] }, undefined)], ['h']);
  assert.deepEqual([...sharedWithOf({ chatId: 'f', parentChatId: null, sharedWith: ['h'] }, new Set())], [], 'a share made here wins over the row');
});

test('the profile item, the first-run question, the tiles and the last-seen time', () => {
  assert.equal(itemText(view('h')), 'Home');
  assert.equal(itemText(view('o')), 'Other');
  assert.equal(itemText(view('gone')), 'Home', 'an unknown active profile reads as the home profile');
  assert.equal(itemText({ profiles: [], homeId: null, activeId: null }), 'Who uses this Helm?');
  const unnamed = { profiles: [sum('h', 'Me', { isHome: true, needsName: true })], homeId: 'h', activeId: 'h' };
  assert.deepEqual([itemText(unnamed), needsFirstRun(unnamed), needsFirstRun(view('h')), needsFirstRun({ profiles: [], homeId: null, activeId: null }), needsFirstRun(null)], ['Who uses this Helm?', true, false, true, false]);
  assert.deepEqual([tileCounts({ chats: 1, running: 0 }), tileCounts({ chats: 12, running: 2 })], ['1 chat · 0 running', '12 chats · 2 running']);
  assert.deepEqual([stepTile(3, -1, 1), stepTile(3, -1, -1), stepTile(3, 2, 1), stepTile(3, 0, -1), stepTile(3, 1, 1), stepTile(0, 0, 1)], [0, 2, 0, 2, 2, -1]);
  assert.deepEqual([initialOf('sam'), initialOf('  éa'), initialOf('')], ['S', 'É', '?']);
  assert.deepEqual([lastSeenMs('2026-11-10T11:00:00.000Z'), lastSeenMs(5), lastSeenMs('never'), lastSeenMs(''), lastSeenMs(null)], [Date.parse('2026-11-10T11:00:00Z'), 5, null, null, null]);
});

test('Move to profile and Share with: the choices, the current one marked, nothing when there is only one profile', () => {
  const snap = { loaded: true, view: view('h'), ids: null, shared: new Map([['c', new Set(['x'])]]), people: null, error: null };
  const rows = [{ chatId: 'a', parentChatId: null }, { chatId: 'b', parentChatId: null, profileId: 'o', sharedWith: ['h'] }, { chatId: 'c', parentChatId: null, profileId: 'h', sharedWith: ['o'] }];
  const cur = (id: string) => profileChoices(snap, id, rows).filter((c) => c.current).map((c) => c.id);
  assert.deepEqual([cur('a'), cur('b'), cur('c')], [['h'], ['o'], ['h']], 'the row names the profile; a row that does not belongs to the one showing');
  assert.deepEqual(profileChoices(snap, 'a', rows).map((c) => c.id), ['h', 'o'], 'home first');
  assert.deepEqual(profileChoices({ ...snap, view: { ...view('h'), profiles: [sum('h', 'Home', { isHome: true })] } }, 'a', rows), []);
  // Share with: the chat's own profile is not offered; ticked where it is shared (a share made here wins over the row).
  const shares = (id: string) => shareChoices(snap, id, rows).map((c) => `${c.id}:${c.shared}`);
  assert.deepEqual([shares('a'), shares('b'), shares('c')], [['o:false'], ['h:true'], ['o:false']]);
  assert.deepEqual(shareChoices({ ...snap, view: { ...view('h'), profiles: [sum('h', 'Home', { isHome: true })] } }, 'a', rows), []);
});

/** A scripted host: the ops the store calls, recording what it was asked. */
function host(opts: { online?: boolean } = {}) {
  const calls: [string, unknown][] = [];
  let active: string | null = null;
  const api = (async (op: string, args: unknown) => {
    calls.push([op, args]);
    switch (op) {
      case 'profiles_list':
        return view(active);
      case 'profiles_chat_ids':
        return { chatIds: (args as { profileId: string }).profileId === 'o' ? ['b', 's'] : ['a'] };
      case 'profiles_assign_chats':
      case 'profiles_share_chats':
        return view(active);
      case 'relay_connection_get':
        return { hasAddress: true, hasFallback: false, hasToken: true, state: opts.online === false ? 'offline' : 'online' };
      case 'peer_people':
        return { people };
      default:
        throw new ApiError('unknown_op', op);
    }
  }) as Api;
  return { api, calls, setActive: (id: string | null) => (active = id) };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

test("the store reads the active profile's chats, follows chat records, shares a chat, and follows the host", async () => {
  const h = host();
  h.setActive('o');
  const bus = new EventBus();
  const store = new ProfilesStore({ api: h.api, bus });
  store.start();
  await settle();
  assert.equal(store.snapshot.view?.activeId, 'o');
  assert.deepEqual([...(store.snapshot.ids ?? [])], ['b', 's'], 'own and shared, as the host lists them');
  assert.equal(store.snapshot.people, null, 'the people on the hub are read only when asked');
  assert.ok(!h.calls.some(([op]) => op.startsWith('peer_') || op.startsWith('household_')), 'nothing about the household is polled');

  // The record says whose a chat is: owned or shared joins the list, anything else leaves it.
  const fresh = (id: string, over: Record<string, unknown> = {}) => ({ id, parentChatId: null, sideSourceId: null, createdAt: Date.now(), archived: false, ...over });
  const ids = () => [...(store.snapshot.ids ?? [])].sort().join('');
  for (const c of [fresh('n1', { profileId: 'o', sharedWith: [] }), fresh('n2', { profileId: 'h', sharedWith: ['o'] }), fresh('n3', { profileId: 'h', sharedWith: [] }), fresh('s', { profileId: 'h', sharedWith: [] })]) bus.dispatch({ kind: 'chat', chat: c });
  assert.equal(ids(), 'bn1n2', 'n1 and n2 joined, n3 never did, s (unshared) left');
  // A host that does not name the profile: a chat started now at the top level joins, one started from a listed chat joins, others do not.
  for (const c of [fresh('p1'), fresh('p1'), fresh('old', { createdAt: 1 }), fresh('kid', { parentChatId: 'b' }), fresh('side', { sideSourceId: 'b' }), fresh('kid2', { parentChatId: 'zz', createdAt: 1 })]) bus.dispatch({ kind: 'chat', chat: c });
  assert.equal(ids(), 'bkidn1n2p1');

  await store.share('b', 'h', true, new Set());
  assert.deepEqual([...(store.snapshot.shared.get('b') ?? [])], ['h'], 'the mark shows at once');
  assert.deepEqual(h.calls.filter(([op]) => op === 'profiles_share_chats').map(([, a]) => a), [{ chatIds: ['b'], profileId: 'h', shared: true }]);

  const before = h.calls.filter(([op]) => op === 'profiles_list').length;
  bus.dispatch({ kind: 'profiles_changed' });
  await settle();
  assert.ok(h.calls.filter(([op]) => op === 'profiles_list').length > before, 'the list is read again');
  assert.deepEqual([...(store.snapshot.ids ?? [])], ['b', 's']);

  // With no active profile there is no list to keep.
  h.setActive(null);
  bus.dispatch({ kind: 'profiles_changed' });
  await settle();
  assert.equal(store.snapshot.ids, null);
  bus.dispatch({ kind: 'chat', chat: fresh('n9') });
  assert.equal(store.snapshot.ids, null);
  store.stop();
});

test('the people on the hub are read when asked, and none while the hub is not connected', async () => {
  const on = new ProfilesStore({ api: host().api, bus: new EventBus() });
  await on.loadPeople();
  assert.deepEqual(on.snapshot.people?.map((p) => p.name), ['Home', 'Other']);
  const off = new ProfilesStore({ api: host({ online: false }).api, bus: new EventBus() });
  await off.loadPeople();
  assert.equal(off.snapshot.people, null);
});
