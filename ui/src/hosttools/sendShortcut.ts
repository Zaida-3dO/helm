// The send shortcut follows the setting live: read once, then re-applied whenever a `preferences`
// event names it (the same mechanism as the colour mode in shell/theme.ts). A chat changing it
// through app_update_settings emits that event, so no reload is needed.

import type { SendShortcut } from '../composer/keymap.ts';

export const DEFAULT_SEND_SHORTCUT: SendShortcut = 'mod-enter';

/** The stored value; anything missing or unreadable is the default. */
export function parseSendShortcut(v: unknown): SendShortcut {
  return v === 'enter' || v === 'mod-enter' ? v : DEFAULT_SEND_SHORTCUT;
}

type Bus = { on: (kind: string, h: (e: Record<string, unknown>) => void) => () => void };
type ApiFn = <T = unknown>(op: string, args?: unknown) => Promise<T>;

/** Read `sendShortcut`, hand it to `set`, and again on every change. Returns a stop function. */
export function followSendShortcut(api: ApiFn, bus: Bus, set: (s: SendShortcut) => void): () => void {
  let live = true;
  api<unknown>('preference_get', { key: 'sendShortcut' })
    .then((v) => {
      if (live) set(parseSendShortcut(v));
    })
    .catch(() => undefined);
  const off = bus.on('preferences', (e) => {
    const changed = (e.changed ?? {}) as Record<string, unknown>;
    if ('sendShortcut' in changed) set(parseSendShortcut(changed.sendShortcut));
  });
  return () => {
    live = false;
    off();
  };
}
