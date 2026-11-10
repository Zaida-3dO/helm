// The dev fake host's devices (the device toggle and another computer's chats in a plain browser).
// Dev builds only.
//
// The page URL picks the state:
//   (nothing)          this PC, a laptop that is online and a studio PC that is off
//   ?devices=none      this PC alone: no toggle
//
// The chosen device is kept by the fake host's preferences, like any other.

import { ApiError } from '../api/client.ts';
import type { DeviceChats, DeviceInfo, RemoteChatRecord, RemoteProject } from '../devices/api.ts';
import { chat } from './fixtures.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const MIN = 60_000;
const SELF = 'dev-desk';
const LAPTOP = 'dev-laptop';
const STUDIO = 'dev-studio';

function param(name: string): string | null {
  return typeof location === 'undefined' ? null : new URLSearchParams(location.search).get(name);
}

/** The hub person each fake profile stands for. */
const PERSON_OF: Record<string, string> = { 'prof-home': 'p-self', 'prof-sam': 'p-other' };

export function devicesFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const mode = param('devices');
  const t0 = Date.now();
  let selfName = 'Desk';
  const devices = (): DeviceInfo[] => [
    { uid: SELF, name: selfName, person: 'Alex', online: true, lastSeenAt: new Date().toISOString(), run: true, isSelf: true },
    ...(mode === 'none'
      ? []
      : [
          { uid: LAPTOP, name: 'Laptop', person: 'Sam', online: true, lastSeenAt: new Date().toISOString(), run: true, isSelf: false },
          { uid: STUDIO, name: 'Studio', person: 'Alex', online: false, lastSeenAt: new Date(t0 - 95 * MIN).toISOString(), run: false, isSelf: false },
        ]),
  ];

  const projects: Record<string, RemoteProject[]> = {
    [LAPTOP]: [
      { id: 'lp-shop', name: 'Shop', colour: 'teal', kind: 'folder' },
      { id: 'lp-notes', name: 'Notes', colour: null, kind: 'folder' },
    ],
    [STUDIO]: [{ id: 'sp-render', name: 'Render farm', colour: 'purple', kind: 'folder' }],
  };

  const remoteChat = (device: string, id: string, projectId: string, title: string, ago: number, over: Partial<RemoteChatRecord>): RemoteChatRecord => ({
    ...chat({ id, projectId, title, cwd: `C:/Users/me/${projectId}`, status: 'idle', createdAt: t0 - ago * MIN, updatedAt: t0 - ago * MIN, lastActivityAt: t0 - ago * MIN }),
    hostDeviceId: device,
    personId: 'p-other',
    sharedWith: [],
    ...over,
  });
  const remoteChats = (device: string): RemoteChatRecord[] =>
    device === LAPTOP
      ? [
          remoteChat(device, 'rc-1', 'lp-shop', 'Plan the shop banner', 12, { status: 'running', needsAttention: true }),
          remoteChat(device, 'rc-2', 'lp-shop', 'Order spreadsheet', 90, {}),
          remoteChat(device, 'rc-3', 'lp-notes', 'Birthday card ideas', 26 * 60, { sharedWith: ['p-self'] }),
          remoteChat(device, 'rc-4', 'lp-notes', 'Garden plan', 3 * 24 * 60, { archived: true }),
          remoteChat(device, 'rc-5', 'lp-shop', 'Your quote for the van', 40, { personId: 'p-self' }),
        ]
      : device === STUDIO
        ? [
            remoteChat(device, 'rs-1', 'sp-render', 'Queue overnight renders', 300, { personId: 'p-self' }),
            remoteChat(device, 'rs-2', 'sp-render', 'Colour profile notes', 2000, { personId: 'p-self' }),
          ]
        : [];

  const handler: FakeOpHandler = (op, a, ctx) => {
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
        const person = PERSON_OF[String(a.profileId)];
        const wantArchived = a.archived === true;
        const mine = remoteChats(device).filter((c) => !!person && (c.personId === person || c.sharedWith?.includes(person)) && c.archived === wantArchived);
        const offset = Number(a.offset ?? 0);
        const limit = Math.min(Number(a.limit ?? 200), 200);
        const page = mine.slice(offset, offset + limit);
        const used = new Set(page.map((c) => c.projectId));
        const out: DeviceChats = { chats: page, projects: (projects[device] ?? []).filter((p) => used.has(p.id)), more: offset + limit < mine.length };
        return out;
      }
      case 'devices_create_chat':
        throw new ApiError('not_implemented', 'The dev fake host cannot start a chat on another device.');
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['devices_', 'device_'], handler };
}
