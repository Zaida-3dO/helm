// "Scroll to the latest message when I send": on unless the reader turned it off in Settings → Chat.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';

/** The stored value as the setting: on for anything but an explicit `false`. */
export function parseScrollOnSend(v: unknown): boolean {
  return v !== false;
}

/** `scrollOnSend`, live: re-read when the preference changes. */
export function useScrollOnSend(): boolean {
  const shell = useShell();
  const [on, setOn] = useState(true);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'scrollOnSend' })
      .then((v) => setOn(parseScrollOnSend(v)))
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const c = (e as { changed?: Record<string, unknown> }).changed ?? {};
      if ('scrollOnSend' in c) setOn(parseScrollOnSend(c.scrollOnSend));
    });
  }, [shell]);
  return on;
}
