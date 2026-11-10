// The dev fake host's devices (the device toggle and another computer's chats in a plain browser).
// Dev builds only.
//
// The page URL picks the state:
//   (nothing)          this PC, a laptop that is online and a studio PC that is off
//   ?devices=none      this PC alone: no toggle
//   ?devices=offline | readonly | reconnecting
//                      the laptop starts in that state (the studio PC stays off)
//
// The chosen device is kept by the fake host's preferences, like any other. In the dev console,
// `__helmFakeDevices.set('dev-laptop', 'offline')` switches a device to any state while the page is
// open (`live`, `readonly`, `offline`, `reconnecting`).
//
// Chats of another device (`rc-*`, `rs-*`, and the ones made with `devices_create_chat`) are made
// by the fake host on first use from `remoteFakeChat`; changes to a device that cannot take them
// are refused with the words the real host uses (`remoteFakeRefusal`).

import { ApiError } from '../api/client.ts';
import type { DeviceState } from '../api/devices.ts';
import type { ModelCatalogue } from '../api/types.ts';
import type { DeviceChats, DeviceInfo, RemoteChatRecord, RemoteProject } from '../devices/api.ts';
import { remoteChatState } from '../devices/state.ts';
import { chat } from './fixtures.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const MIN = 60_000;
const SELF = 'dev-desk';
const LAPTOP = 'dev-laptop';
const STUDIO = 'dev-studio';

function param(name: string): string | null {
  return typeof location === 'undefined' ? null : new URLSearchParams(location.search).get(name);
}

/** A state set from the page URL or the dev console; a device not named here follows its own flags. */
const forced = new Map<string, DeviceState>();
/** Chats made on another device while the page is open. */
const created: RemoteChatRecord[] = [];
let tell: (() => void) | null = null;
const started = Date.now();

function baseDevices(selfName: string, mode: string | null): DeviceInfo[] {
  const stateOf = (d: DeviceInfo): DeviceInfo => {
    const state = forced.get(d.uid);
    if (!state) return d;
    return { ...d, state, online: state !== 'offline', run: state !== 'readonly' };
  };
  return [
    { uid: SELF, name: selfName, person: 'Alex', online: true, lastSeenAt: new Date().toISOString(), run: true, isSelf: true },
    ...(mode === 'none'
      ? []
      : [
          stateOf({ uid: LAPTOP, name: 'Laptop', person: 'Sam', online: true, lastSeenAt: new Date().toISOString(), run: true, isSelf: false }),
          stateOf({ uid: STUDIO, name: 'Studio', person: 'Alex', online: false, lastSeenAt: new Date(started - 95 * MIN).toISOString(), run: false, isSelf: false }),
        ]),
  ];
}

const PROJECTS: Record<string, RemoteProject[]> = {
  [LAPTOP]: [
    { id: 'lp-shop', name: 'Shop', colour: 'teal', kind: 'folder' },
    { id: 'lp-notes', name: 'Notes', colour: null, kind: 'folder' },
  ],
  [STUDIO]: [{ id: 'sp-render', name: 'Render farm', colour: 'purple', kind: 'folder' }],
};

function fixedChats(device: string): RemoteChatRecord[] {
  const remoteChat = (id: string, projectId: string, title: string, ago: number, over: Partial<RemoteChatRecord>): RemoteChatRecord => ({
    ...chat({ id, projectId, title, cwd: `C:/Users/me/${projectId}`, status: 'idle', createdAt: started - ago * MIN, updatedAt: started - ago * MIN, lastActivityAt: started - ago * MIN }),
    hostDeviceId: device,
    personId: 'p-other',
    sharedWith: [],
    ...over,
  });
  return device === LAPTOP
    ? [
        remoteChat('rc-1', 'lp-shop', 'Plan the shop banner', 12, { status: 'running', needsAttention: true }),
        remoteChat('rc-2', 'lp-shop', 'Order spreadsheet', 90, {}),
        remoteChat('rc-3', 'lp-notes', 'Birthday card ideas', 26 * 60, { sharedWith: ['p-self'] }),
        remoteChat('rc-4', 'lp-notes', 'Garden plan', 3 * 24 * 60, { archived: true }),
        remoteChat('rc-5', 'lp-shop', 'Your quote for the van', 40, { personId: 'p-self' }),
      ]
    : device === STUDIO
      ? [
          remoteChat('rs-1', 'sp-render', 'Queue overnight renders', 300, { personId: 'p-self' }),
          remoteChat('rs-2', 'sp-render', 'Colour profile notes', 2000, { personId: 'p-self' }),
        ]
      : [];
}

/** Why a change to a chat on `deviceUid` is refused right now, or null when the device takes it. */
export function remoteFakeRefusal(deviceUid: string): { code: string; message: string } | null {
  const info = baseDevices('Desk', param('devices')).find((d) => d.uid === deviceUid);
  if (!info || info.isSelf) return null;
  const st = remoteChatState(info, Date.now());
  if (st.live) return null;
  return { code: st.state === 'readonly' ? 'not_allowed' : st.state, message: st.text ?? '' };
}

/** The models a fake device offers (its own list, apart from this PC's). */
export function remoteFakeCatalogue(deviceUid: string): ModelCatalogue {
  const name = deviceUid === STUDIO ? 'Studio' : 'Laptop';
  return {
    harness: 'claude',
    models: [
      { id: 'claude-opus-5-5', name: `Opus 5.5 (${name})`, description: 'Most capable' },
      { id: 'claude-haiku-5', name: `Haiku 5 (${name})`, description: 'Fastest' },
    ],
    efforts: ['low', 'medium', 'high'],
    defaultEffort: 'high',
    source: 'fallback',
    local: [],
  } as ModelCatalogue;
}

/** A chat of another device by id, for the fake host to keep; null when there is no such chat. */
export function remoteFakeChat(id: string): RemoteChatRecord | null {
  const made = created.find((c) => c.id === id);
  if (made) return made;
  for (const device of [LAPTOP, STUDIO]) {
    const found = fixedChats(device).find((c) => c.id === id);
    if (found) return found;
  }
  return null;
}

/** The hub person each fake profile stands for. */
const PERSON_OF: Record<string, string> = { 'prof-home': 'p-self', 'prof-sam': 'p-other' };

export function devicesFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const mode = param('devices');
  let selfName = 'Desk';
  if ((mode === 'offline' || mode === 'readonly' || mode === 'reconnecting') && !forced.has(LAPTOP)) forced.set(LAPTOP, mode);
  const devices = (): DeviceInfo[] => baseDevices(selfName, mode);
  const remoteChats = (device: string): RemoteChatRecord[] => [...fixedChats(device), ...created.filter((c) => c.hostDeviceId === device)];

  const handler: FakeOpHandler = (op, a, ctx) => {
    tell = () => ctx.emit({ kind: 'devices_changed' });
    switch (op) {
      case 'device_get':
        return { uid: SELF, name: selfName };
      case 'device_rename': {
        const n = String(a.name ?? '').trim();
        if (!n || n.length > 40) throw new ApiError('bad_args', "A device's name is 1 to 40 characters.");
        selfName = n;
        ctx.emit({ kind: 'device_changed' });
        ctx.emit({ kind: 'devices_changed' });
        return { uid: SELF, name: selfName };
      }
      case 'devices_list':
        return { self: SELF, devices: devices() };
      case 'devices_chats': {
        const device = String(a.deviceUid);
        if (!devices().some((d) => d.uid === device)) throw new ApiError('not_found', 'That device is not on the hub.');
        // Without a profile every chat of the device is listed, with every project it has.
        const everything = a.profileId === undefined;
        const person = PERSON_OF[String(a.profileId)];
        const wantArchived = a.archived === true;
        const mine = remoteChats(device).filter(
          (c) => (everything || (!!person && (c.personId === person || c.sharedWith?.includes(person)))) && c.archived === wantArchived,
        );
        const offset = Number(a.offset ?? 0);
        const limit = Math.min(Number(a.limit ?? 200), 200);
        const page = mine.slice(offset, offset + limit);
        const used = new Set(page.map((c) => c.projectId));
        const out: DeviceChats = { chats: page, projects: (PROJECTS[device] ?? []).filter((p) => everything || used.has(p.id)), more: offset + limit < mine.length };
        return out;
      }
      case 'devices_open_chat':
        return {};
      case 'devices_create_chat': {
        const device = String(a.deviceUid);
        const refusal = remoteFakeRefusal(device);
        if (refusal) throw new ApiError(refusal.code, refusal.message);
        const project = (PROJECTS[device] ?? []).find((p) => p.id === a.projectId);
        if (!project) throw new ApiError('not_found', 'That project is not on that device.');
        const at = Date.now();
        const made: RemoteChatRecord = {
          ...chat({
            id: `rc-new-${created.length + 1}`,
            projectId: project.id,
            title: 'New chat',
            cwd: `C:/Users/me/${project.id}`,
            model: typeof a.model === 'string' ? a.model : null,
            effort: typeof a.effort === 'string' ? a.effort : null,
            createdAt: at,
            updatedAt: at,
            lastActivityAt: at,
          }),
          hostDeviceId: device,
          personId: 'p-self',
          sharedWith: [],
        };
        created.push(made);
        ctx.emit({ kind: 'devices_chats_changed', deviceUid: device });
        return { chatId: made.id };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['devices_', 'device_'], handler };
}

if (typeof window !== 'undefined') {
  (window as unknown as { __helmFakeDevices?: { set: (uid: string, state: DeviceState) => void } }).__helmFakeDevices = {
    set: (uid, state) => {
      forced.set(uid, state);
      tell?.();
    },
  };
}
