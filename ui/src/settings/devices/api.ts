// The device ops Settings → Devices uses. This PC's name and the list of devices come from the
// sidebar's device module (`ui/src/devices/api.ts`); what is added here is the switch that lets
// other devices run chats on this PC.

import type { Api } from '../../api/client.ts';
import { deviceOps } from '../../devices/api.ts';

export type { DeviceInfo as DeviceRow } from '../../devices/api.ts';

/** The user-only preference behind "Let other devices use chats on this PC"; on when never set. */
export const RUN_FROM_OTHERS_KEY = 'deviceRunFromOthers';

export function devicesOps(api: Api) {
  const devices = deviceOps(api);
  return {
    self: devices.self,
    rename: devices.rename,
    list: devices.list,
    runFromOthers: () => api<unknown>('preference_get', { key: RUN_FROM_OTHERS_KEY }).then((v) => v !== false),
    setRunFromOthers: (on: boolean) => api<unknown>('preference', { key: RUN_FROM_OTHERS_KEY, value: on }),
  };
}
