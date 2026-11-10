// Settings → Devices, without React: the plain-words status of a device and the name rule.

import type { DeviceRow } from './api.ts';

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

/** The longest device name the host keeps. */
export const NAME_MAX = 40;

/** "online", "last seen 5 minutes ago", "never connected". */
export function presence(d: Pick<DeviceRow, 'online' | 'lastSeenAt'>, now: number): string {
  if (d.online) return 'online';
  const at = d.lastSeenAt ? Date.parse(d.lastSeenAt) : NaN;
  if (Number.isNaN(at)) return 'never connected';
  const ago = Math.max(0, now - at);
  if (ago < MIN) return 'last seen just now';
  const unit = (n: number, word: string) => `last seen ${n} ${word}${n === 1 ? '' : 's'} ago`;
  if (ago < HOUR) return unit(Math.floor(ago / MIN), 'minute');
  if (ago < DAY) return unit(Math.floor(ago / HOUR), 'hour');
  return unit(Math.floor(ago / DAY), 'day');
}

/** Whether a typed name can be saved (1 to 40 characters once trimmed). */
export function nameOk(name: string): boolean {
  const n = name.trim();
  return n.length > 0 && n.length <= NAME_MAX;
}

/** The other devices, online ones first, then by name; this PC is left out. */
export function otherDevices(devices: DeviceRow[]): DeviceRow[] {
  return devices
    .filter((d) => !d.isSelf)
    .sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name));
}
