// Registers "Go back to before this message" on the user's own messages and shows its confirmation.
// Mounted once, beside the other slot registrations.

import { useEffect, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { useShell } from '../shell/context.ts';
import { registerMessageAction } from '../transcript/slots.ts';
import { IconUndo } from '../ui/icons.tsx';
import { CheckpointDialog } from './CheckpointDialog.tsx';
import { canGoBackTo } from './model.ts';

export const CHECKPOINT_ACTION_ID = 'go-back-to-here';

export function CheckpointSlot() {
  const shell = useShell();
  const [target, setTarget] = useState<{ chatId: string; row: MessageRow } | null>(null);
  useEffect(
    () =>
      registerMessageAction({
        id: CHECKPOINT_ACTION_ID,
        label: 'Go back to before this message',
        icon: IconUndo,
        order: 15,
        appliesTo: canGoBackTo,
        run: (row, ctx) => setTarget({ chatId: ctx.chatId, row }),
      }),
    [],
  );
  if (!target) return null;
  return <CheckpointDialog api={shell.api} chatId={target.chatId} row={target.row} onOpen={(id) => shell.select(id)} onClose={() => setTarget(null)} />;
}
