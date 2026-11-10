// The three household switches. All are off until turned on, and each one counts only while the one
// above it is on, so turning one off dims the ones below. They are the user's alone: no agent tool
// reaches them.

import { useState } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { Sharing } from '../api/profiles.ts';
import { Checkbox, SettingRow } from '../ui/index.ts';
import { useProfiles } from './hooks.ts';
import { errorText } from './store.ts';

export const SWITCHES: { key: keyof Sharing; title: string; description: string; needs?: keyof Sharing }[] = [
  {
    key: 'chats',
    title: 'Share my chats with the household',
    description: 'People on your hub can see the titles, status and last update of your chats, and whether one needs you. Nothing is copied: they read it from this computer while it is on.',
  },
  {
    key: 'transcripts',
    title: 'Let household members read my transcripts',
    description: 'They can open one of your chats and read it, read only.',
    needs: 'chats',
  },
  {
    key: 'messages',
    title: 'Household members can message my chats',
    description: "They can send a message into one of your chats. It shows as written by them, never as you, and the chat runs here on this computer's Claude sign-in.",
    needs: 'transcripts',
  },
];

export function SharingSwitches({ api, bus }: { api: Api; bus: EventBus }) {
  const { store, snap } = useProfiles(api, bus);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const change = async (key: keyof Sharing, on: boolean) => {
    setBusy(true);
    try {
      await store.household.set({ [key]: on });
      setError(null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div data-testid="household-switches">
      {SWITCHES.map((s) => (
        <SettingRow key={s.key} testId={`household-${s.key}`} title={s.title} description={s.description}>
          <Checkbox
            checked={snap.sharing[s.key]}
            disabled={busy || (s.needs !== undefined && !snap.sharing[s.needs])}
            aria-label={s.title}
            data-testid={`household-${s.key}-box`}
            onChange={(on) => void change(s.key, on)}
          />
        </SettingRow>
      ))}
      {error && (
        <div className="ui-field-error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
