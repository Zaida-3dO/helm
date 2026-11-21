// Settings → Devices, without React: the plain-words status of a device, the name rule, and the
// words for a port forwarded to this PC (also used by a remote chat's bar and its toast).

import type { Forward } from '../../devices/api.ts';
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

/** Who a computer acts for on the hub: "Pat, Sam". Falls back to the one person it was made
 *  for when its report carries no list (an older PC that has not announced its profiles). */
export function peopleLine(d: Pick<DeviceRow, 'person' | 'people'>): string {
  const names = (d.people ?? []).map((p) => p.person).filter((n) => n);
  if (names.length) return names.join(', ');
  return d.person ?? 'Unknown person';
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

/** "Port 5173 on Desk opens here at 127.0.0.1:5173", with the title when the agent gave one. */
export function forwardLine(f: Pick<Forward, 'port' | 'localPort' | 'deviceName' | 'title'>): string {
  const where = f.localPort === f.port ? `127.0.0.1:${f.localPort}` : `127.0.0.1:${f.localPort} (${f.port} was taken here)`;
  const title = f.title.trim() ? ` (${f.title.trim()})` : '';
  return `Port ${f.port} on ${f.deviceName}${title} opens here at ${where}`;
}

/** How a forward is used right now: "2 connections open", "idle for 5 minutes". */
export function forwardUse(f: Pick<Forward, 'streams' | 'lastUsedAt' | 'openedAt'>, now: number): string {
  if (f.streams > 0) return `${f.streams} connection${f.streams === 1 ? '' : 's'} open`;
  const since = Math.max(f.lastUsedAt || 0, f.openedAt || 0);
  const mins = Math.floor(Math.max(0, now - since) / MIN);
  return mins < 1 ? 'idle' : `idle for ${mins} minute${mins === 1 ? '' : 's'}`;
}

/** The ports forwarded to this PC, one row each, by PC then port; `chatId` keeps one chat's. */
export function forwardRows(forwards: readonly Forward[], now: number, chatId?: string): { forward: Forward; line: string; use: string }[] {
  return forwards
    .filter((f) => chatId === undefined || f.chatId === chatId)
    .slice()
    .sort((a, b) => a.deviceName.localeCompare(b.deviceName) || a.port - b.port)
    .map((forward) => ({ forward, line: forwardLine(forward), use: forwardUse(forward, now) }));
}
