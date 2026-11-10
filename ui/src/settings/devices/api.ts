// The device ops Settings → Devices uses. This PC's name and the list of devices come from the
// sidebar's device module (`ui/src/devices/api.ts`); what is added here are the switches that let
// other devices, and people in the household, run chats on this PC.

import type { Api } from '../../api/client.ts';
import { deviceOps } from '../../devices/api.ts';

export type { DeviceInfo as DeviceRow } from '../../devices/api.ts';

/** The user-only preference behind "Let other devices use chats on this PC"; on when never set. */
export const RUN_FROM_OTHERS_KEY = 'deviceRunFromOthers';

/** The user-only preference behind "Let people in my household run their chats on this PC"; on when never set. */
export const HOUSEHOLD_RUN_KEY = 'deviceHouseholdRun';

export function devicesOps(api: Api) {
  const devices = deviceOps(api);
  return {
    self: devices.self,
    rename: devices.rename,
    list: devices.list,
    runFromOthers: () => api<unknown>('preference_get', { key: RUN_FROM_OTHERS_KEY }).then((v) => v !== false),
    setRunFromOthers: (on: boolean) => api<unknown>('preference', { key: RUN_FROM_OTHERS_KEY, value: on }),
    householdRun: () => api<unknown>('preference_get', { key: HOUSEHOLD_RUN_KEY }).then((v) => v !== false),
    setHouseholdRun: (on: boolean) => api<unknown>('preference', { key: HOUSEHOLD_RUN_KEY, value: on }),
  };
}
