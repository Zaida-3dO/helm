// The star after the model control: make this chat's model and effort the default for new chats
// (the same default Settings → Runtime holds). Filled while they already are.

import { useEffect, useState } from 'react';
import type { ChatRecord } from '../api/types.ts';
import type { RuntimeSettings } from '../settings/runtime/model.ts';
import { useShell } from '../shell/context.ts';
import { Button } from '../ui/controls.tsx';
import { IconStar } from '../ui/icons.tsx';

export function DefaultStar({ chat }: { chat: ChatRecord }) {
  const shell = useShell();
  const [saved, setSaved] = useState<RuntimeSettings | null>(null);
  useEffect(() => {
    let live = true;
    shell
      .api<RuntimeSettings>('runtime_settings_get', {})
      .then((s) => live && setSaved(s))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [shell]);
  if (!saved || !chat.model) return null;
  const isDefault = saved.model === chat.model && saved.effort === (chat.effort ?? saved.effort);
  return (
    <Button
      variant="icon"
      className={`composer-default-star${isDefault ? ' on' : ''}`}
      label={isDefault ? 'This is the default for new chats' : 'Make this the default for new chats'}
      title={isDefault ? 'The default for new chats' : 'Make default for new chats'}
      pressed={isDefault}
      disabled={isDefault}
      data-testid="make-default"
      onClick={() => {
        shell
          .api<RuntimeSettings>('runtime_settings_save', { model: chat.model, ...(chat.effort ? { effort: chat.effort } : {}) })
          .then(setSaved)
          .catch(() => undefined);
      }}
    >
      <IconStar size={13} filled={isDefault} />
    </Button>
  );
}
