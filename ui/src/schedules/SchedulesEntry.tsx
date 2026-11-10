// The sidebar's Schedules view. It opens Settings on the Schedules pane, counts the reminders that
// are waiting, and posts a notice when a reminder comes due while the app is open. The sidebar's
// tile row mounts `useSchedulesEntry`, so the hook is always on screen and keeps the schedule
// store reading; `SchedulesEntry` is the same thing as a footer-style button.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { requestPane } from '../settings/registry.ts';
import { IconClock } from '../ui/icons.tsx';
import { toast } from '../ui/index.ts';
import { reminderText } from './model.ts';
import { scheduleStoreFor } from './store.ts';

export const SCHEDULES_PANE = 'schedules';

/** How long a reminder notice stays up before it falls back to the count. */
export const REMINDER_TOAST_MS = 15_000;

/** How many reminders wait, and how to open Settings on the Schedules pane. */
export function useSchedulesEntry(
  api: Api,
  bus: EventBus,
  openSettings: () => void,
  onOpenChat: (chatId: string) => void,
): { waiting: number; open: () => void } {
  const store = scheduleStoreFor(api, bus);
  const open = () => {
    requestPane(SCHEDULES_PANE);
    openSettings();
  };
  useEffect(() => {
    store.start();
    return store.onReminder((r) =>
      toast.show({
        kind: 'info',
        text: reminderText(r),
        ttlMs: REMINDER_TOAST_MS,
        action: r.chatId ? { label: 'Open chat', run: () => onOpenChat(r.chatId as string) } : { label: 'Open', run: open },
      }),
    );
    // `open` and `onOpenChat` are stable enough for a notice that lasts seconds.
  }, [store]);
  const waiting = useSyncExternalStore(store.subscribe, () => store.reminders.length);
  return { waiting, open };
}

export function SchedulesEntry({
  api,
  bus,
  openSettings,
  onOpenChat,
}: {
  api: Api;
  bus: EventBus;
  openSettings: () => void;
  onOpenChat: (chatId: string) => void;
}) {
  const { waiting, open } = useSchedulesEntry(api, bus, openSettings, onOpenChat);
  return (
    <button type="button" className="sb-foot" onClick={open} data-testid="nav-schedules">
      <IconClock />
      <span>Schedules</span>
      {waiting > 0 && (
        <span className="sb-nav-count" aria-label={`${waiting} reminders waiting`} data-testid="nav-schedules-count">
          {waiting}
        </span>
      )}
    </button>
  );
}
