// The sidebar's Schedules view. It opens and closes the schedules view in the main area, counts the
// reminders that are waiting, and posts a notice when a reminder comes due while the app is open.
// The sidebar's tile row mounts `useSchedulesEntry`, so the hook is always on screen and keeps the
// schedule store reading.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { toast } from '../ui/index.ts';
import { reminderText } from './model.ts';
import { scheduleStoreFor } from './store.ts';
import { schedulesView } from './view.ts';

/** How long a reminder notice stays up before it falls back to the count. */
export const REMINDER_TOAST_MS = 15_000;

/** How many reminders wait, whether the schedules view is open, and how to toggle it. */
export function useSchedulesEntry(api: Api, bus: EventBus, onOpenChat: (chatId: string) => void): { waiting: number; open: boolean; toggle: () => void } {
  const store = scheduleStoreFor(api, bus);
  useEffect(() => {
    store.start();
    return store.onReminder((r) =>
      toast.show({
        kind: 'info',
        text: reminderText(r),
        ttlMs: REMINDER_TOAST_MS,
        action: r.chatId ? { label: 'Open chat', run: () => onOpenChat(r.chatId as string) } : { label: 'Open', run: schedulesView.show },
      }),
    );
    // `onOpenChat` is stable enough for a notice that lasts seconds.
  }, [store]);
  const waiting = useSyncExternalStore(store.subscribe, () => store.reminders.length);
  const open = useSyncExternalStore(schedulesView.subscribe, schedulesView.isOpen);
  return { waiting, open, toggle: schedulesView.toggle };
}
