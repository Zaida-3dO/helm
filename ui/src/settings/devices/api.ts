// The device ops Settings → Devices uses: this PC's name and the other devices on the hub. Typed by
// hand, like the other device ops. (The sidebar's device toggle reads the same list.)

import type { Api } from '../../api/client.ts';

export interface ThisDevice {
  uid: string;
  name: string;
}

/** One device on the hub, as `devices_list` reports it. */
export interface DeviceRow {
  uid: string;
  name: string;
  /** The person who uses it (the hub's name for them); null when unknown. */
  person: string | null;
  online: boolean;
  /** An ISO time from the hub, or null when it never connected. */
  lastSeenAt: string | null;
  /** Whether it lets other devices run chats on it. */
  run: boolean;
  isSelf: boolean;
}

export interface DeviceList {
  /** This PC's uid. */
  self: string;
  devices: DeviceRow[];
}

/** The user-only preference behind "Let other devices use chats on this PC"; on when never set. */
export const RUN_FROM_OTHERS_KEY = 'deviceRunFromOthers';

export function devicesOps(api: Api) {
  return {
    self: () => api<ThisDevice>('device_get', {}),
    rename: (name: string) => api<ThisDevice>('device_rename', { name }),
    list: () => api<DeviceList>('devices_list', {}),
    runFromOthers: () => api<unknown>('preference_get', { key: RUN_FROM_OTHERS_KEY }).then((v) => v !== false),
    setRunFromOthers: (on: boolean) => api<unknown>('preference', { key: RUN_FROM_OTHERS_KEY, value: on }),
  };
}

export type DevicesOps = ReturnType<typeof devicesOps>;
