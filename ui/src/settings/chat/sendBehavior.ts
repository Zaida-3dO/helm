// Settings → Chat → how messages sent while Claude is working behave. The host stores only the
// keys that differ from the defaults; this reads either that or the full object.

import { HOME_SEND_BEHAVIOR } from '../../api/types.ts';
import type { SendBehavior } from '../../api/generated/index.ts';

export type SendMode = SendBehavior['mode'];
export type LongCommands = SendBehavior['longCommands'];

export const SEND_MODES: SendMode[] = ['queue', 'asap', 'interrupt'];
export const LONG_COMMANDS: LongCommands[] = ['off', 'warn', 'redirect'];
/** `null` is "no limit"; the host accepts 5 to 3600 seconds. */
export const WAIT_LIMITS: (number | null)[] = [null, 10, 30, 60, 120, 300, 600, 1800, 3600];

/** The stored value (a partial object, the full object or nothing) as the full setting. */
export function parseSendBehavior(v: unknown): SendBehavior {
  const out: SendBehavior = { ...HOME_SEND_BEHAVIOR };
  if (!v || typeof v !== 'object') return out;
  const m = v as Record<string, unknown>;
  if (SEND_MODES.includes(m.mode as SendMode)) out.mode = m.mode as SendMode;
  if (LONG_COMMANDS.includes(m.longCommands as LongCommands)) out.longCommands = m.longCommands as LongCommands;
  for (const k of ['showChoices', 'stopSendsWaiting', 'toolStop'] as const) if (typeof m[k] === 'boolean') out[k] = m[k] as boolean;
  if (m.waitLimitSeconds === null) out.waitLimitSeconds = null;
  else if (typeof m.waitLimitSeconds === 'number' && m.waitLimitSeconds >= 5 && m.waitLimitSeconds <= 3600) out.waitLimitSeconds = m.waitLimitSeconds;
  return out;
}

/** Only the keys that differ from the defaults, which is what the host stores. */
export function sendBehaviorDiff(sb: SendBehavior): Partial<SendBehavior> {
  const out: Record<string, unknown> = {};
  for (const k of Object.keys(HOME_SEND_BEHAVIOR) as (keyof SendBehavior)[]) if (sb[k] !== HOME_SEND_BEHAVIOR[k]) out[k] = sb[k];
  return out as Partial<SendBehavior>;
}

export function waitLimitLabel(s: number | null): string {
  if (s === null) return 'No limit';
  const base = s < 60 ? `${s} seconds` : s < 3600 ? `${s / 60} minute${s === 60 ? '' : 's'}` : '1 hour';
  return s === HOME_SEND_BEHAVIOR.waitLimitSeconds ? `${base} (default)` : base;
}
