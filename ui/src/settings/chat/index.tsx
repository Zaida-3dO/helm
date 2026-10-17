// Settings → Chat: the transcript element toggles. Each switch saves at
// once through the generic `preference` op, writing the whole object.

import { useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Checkbox, SettingRow, toast } from '../../ui/index.ts';
import { registerSettingsPane } from '../registry.ts';
import { ELEMENTS, isShown, parseChatElements, type ChatElements } from './elements.ts';

function ChatPane() {
  const shell = useShell();
  const [els, setEls] = useState<ChatElements>({});
  const [ac, setAc] = useState(false);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'chatElements' })
      .then((v) => setEls(parseChatElements(v)))
      .catch(() => undefined);
    void shell
      .api<unknown>('preference_get', { key: 'autocomplete' })
      .then((v) => setAc(Boolean(v && typeof v === 'object' && (v as { enabled?: unknown }).enabled === true)))
      .catch(() => undefined);
  }, [shell]);
  return (
    <div data-testid="settings-chat">
      <SettingRow title="Autocomplete" description="Suggest the rest of the word you are typing; Tab accepts. Uses Claude Code's small model, at most 10 requests a minute.">
        <Checkbox
          checked={ac}
          onChange={(on) => {
            setAc(on);
            shell.bus.dispatch({ kind: 'preferences', changed: { autocomplete: { enabled: on } } });
            void shell.api('preference', { key: 'autocomplete', value: { enabled: on } }).catch((err: unknown) => {
              setAc(!on);
              shell.bus.dispatch({ kind: 'preferences', changed: { autocomplete: { enabled: !on } } });
              toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
            });
          }}
        />
      </SettingRow>
      {ELEMENTS.map((e) => (
        <SettingRow key={e.id} title={e.title} description={e.description}>
          <Checkbox
            checked={isShown(els, e.id)}
            onChange={(on) => {
              const before = els;
              const next: ChatElements = { ...els, [e.id]: on ? 'shown' : 'hidden' };
              setEls(next);
              shell.bus.dispatch({ kind: 'preferences', changed: { chatElements: next } });
              void shell.api('preference', { key: 'chatElements', value: next }).catch((err: unknown) => {
                setEls(before);
                shell.bus.dispatch({ kind: 'preferences', changed: { chatElements: before } });
                toast.show({ kind: 'error', text: `Could not save: ${String((err as Error).message ?? err)}` });
              });
            }}
          />
        </SettingRow>
      ))}
    </div>
  );
}

registerSettingsPane({ id: 'chat', title: 'Chat', order: 30, render: () => <ChatPane /> });
