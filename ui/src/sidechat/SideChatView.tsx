// The side chat's body in its panel tab: the same chat view as the main pane.

import { useMemo } from 'react';
import { ChatView } from '../ChatView.tsx';
import { useShell } from '../shell/context.ts';
import { Flushers } from '../shell/quit.ts';

export function SideChatView({ chatId }: { chatId: string }) {
  const shell = useShell();
  const flushers = useMemo(() => new Flushers(), []);
  return <ChatView key={chatId} chatId={chatId} store={shell.store} ops={shell.ops} flushers={flushers} sendShortcut="mod-enter" />;
}
