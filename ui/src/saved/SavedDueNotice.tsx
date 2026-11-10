// The one notice for saved items whose reminder came due: a lasting toast for all that are due (a
// count when several), not a toast per item that the next one replaces. What came due while no
// window was listening is asked for once this one is. The Saved pane keeps saying "Due now" until
// the user deals with each item. Mounted once in the app frame; it draws nothing itself.

import { useEffect } from 'react';
import { requestPane } from '../settings/registry.ts';
import { SETTINGS_DIALOG } from '../settings/index.ts';
import { useShell } from '../shell/context.ts';
import { toast } from '../ui/index.ts';
import { dueNotice, savedOps } from './model.ts';

export function SavedDueNotice() {
  const shell = useShell();

  useEffect(() => {
    const due = new Map<string, string>();
    const add = (items: { id: string; title: string }[]) => {
      for (const it of items) due.set(it.id, it.title);
      const text = dueNotice([...due.values()]);
      if (text === null) return;
      toast.show({
        kind: 'info',
        text,
        ttlMs: 60_000,
        action: {
          label: 'Open Saved',
          run: () => {
            requestPane('saved');
            shell.openDialog(SETTINGS_DIALOG);
          },
        },
      });
    };
    const off = shell.bus.on('saved_due', (e) => add((e as { items?: { id: string; title: string }[] }).items ?? []));
    savedOps(shell.api)
      .due()
      .then((r) => add(r.items))
      .catch(() => undefined);
    return off;
  }, [shell]);

  return null;
}
