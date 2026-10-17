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
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'chatElements' })
      .then((v) => setEls(parseChatElements(v)))
      .catch(() => undefined);
  }, [shell]);
  return (
    <div data-testid="settings-chat">
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
