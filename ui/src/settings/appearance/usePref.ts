// A preference one Appearance control edits. It loads once, applies at once through the same event
// the host sends after a save (so the interface changes before the write finishes), saves through
// the generic `preference` op, and puts the old value back with a message when the save fails.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { toast } from '../../ui/index.ts';

export function usePref<T>(key: string, parse: (v: unknown) => T, toStored: (v: T) => unknown = (v) => v): [T, (next: T) => void] {
  const shell = useShell();
  const [value, setValue] = useState<T>(() => parse(undefined));
  const latest = useRef(value);
  useEffect(() => {
    let live = true;
    shell
      .api<unknown>('preference_get', { key })
      .then((v) => {
        if (!live) return;
        latest.current = parse(v);
        setValue(latest.current);
      })
      .catch(() => undefined);
    // An agent's change (or another window's) arrives as the host's own event.
    const off = shell.bus.on('preferences', (e) => {
      const changed = (e.changed ?? {}) as Record<string, unknown>;
      if (!(key in changed)) return;
      latest.current = parse(changed[key]);
      setValue(latest.current);
    });
    return () => {
      live = false;
      off();
    };
  }, [shell, key, parse]);
  const set = useCallback(
    (next: T) => {
      const before = latest.current;
      latest.current = next;
      setValue(next);
      shell.bus.dispatch({ kind: 'preferences', changed: { [key]: toStored(next) } });
      shell.api('preference', { key, value: toStored(next) }).catch((e: unknown) => {
        latest.current = before;
        setValue(before);
        shell.bus.dispatch({ kind: 'preferences', changed: { [key]: toStored(before) } });
        toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
      });
    },
    [shell, key, toStored],
  );
  return [value, set];
}
