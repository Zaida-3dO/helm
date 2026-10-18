// The settings-change toast: Undo puts back every value the chat changed
// (all or nothing, and only if unchanged since); Open chat shows the chat that did it.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Toast } from '../ui/misc.tsx';
import { settingsToast } from './settingsToast.ts';
import './hosttools.css';

export function SettingsToast({ api, bus, onOpenChat }: { api: Api; bus: EventBus; onOpenChat: (chatId: string) => void }) {
  const n = useSyncExternalStore(settingsToast.subscribe, settingsToast.current);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => bus.on('external_settings', (e) => (setError(null), settingsToast.show(e))), [bus]);
  if (!n) return null;
  const undo = () =>
    api<unknown>('settings_undo', { changeIds: n.changeIds })
      .then(() => settingsToast.dismiss())
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String((e as { message?: unknown })?.message ?? e)));
  return (
    <div className="ui-toast-slot ui-toast-slot-settings" data-testid="settings-toast">
      <Toast
        kind="info"
        testId="settings-toast-notice"
        action={{ label: 'Undo', onClick: () => void undo() }}
        secondary={{ label: 'Open chat', onClick: () => (settingsToast.dismiss(), onOpenChat(n.chatId)) }}
        onDismiss={() => settingsToast.dismiss()}
      >
        {error ?? n.text}
      </Toast>
    </div>
  );
}
