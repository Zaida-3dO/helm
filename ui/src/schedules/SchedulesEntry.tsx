// The sidebar footer's Schedules entry. It opens Settings on the Schedules pane, shows how many
// reminders are waiting, and posts a notice when a reminder comes due while the app is open.
// Mounted by the sidebar, so it is always on screen and keeps the schedule store reading.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { requestPane } from '../settings/registry.ts';
import { IconClock } from '../ui/icons.tsx';
import { toast } from '../ui/index.ts';
import { reminderText } from './model.ts';
import { scheduleStoreFor } from './store.ts';

export const SCHEDULES_PANE = 'schedules';

/** How long a reminder notice stays up before it falls back to the footer count. */
export const REMINDER_TOAST_MS = 15_000;

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
