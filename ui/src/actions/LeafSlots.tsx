// Registers the actions area's slot contributions with the other areas' registries: quick
// replies above the composer, option buttons under the latest reply, Explain plainly in a reply's
// actions, and the Actions pane in Settings. Mounted once in the app frame; it draws nothing itself.

import { useEffect } from 'react';
import { searchOps } from '../api/search.ts';
import { ActionsPane } from '../settings/actions/ActionsPane.tsx';
import { registerComposerAccessory } from '../composer/slots.ts';
import { registerSettingsPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';
import { registerMessageAction, registerRowFooter } from '../transcript/slots.ts';
import { explainAppliesTo } from './ExplainPlainly.tsx';
import { OptionButtons, QuickReplies } from './Replies.tsx';
import { EXPLAIN_PLAINLY_TEXT, sendActionText } from './send.ts';

export function LeafSlots(): null {
  const shell = useShell();
  useEffect(() => {
    const api = shell.api;
    const offs = [
      registerComposerAccessory({
        id: 'leaf-quick-replies',
        placement: 'above',
        render: (ctx) => <QuickReplies chatId={ctx.chat.id} />,
      }),
      registerRowFooter({
        id: 'leaf-option-buttons',
        appliesTo: (row, ctx) => row.role === 'assistant' && ctx.isLatestAssistant && !ctx.running && !ctx.interactionOpen,
        render: (_row, ctx) => <OptionButtons chatId={ctx.chatId} />,
      }),
      registerMessageAction({
        id: 'leaf-explain-plainly',
        label: 'Explain plainly',
        appliesTo: explainAppliesTo,
        run: (_row, ctx) => {
          sendActionText(api, ctx.chatId, EXPLAIN_PLAINLY_TEXT).catch((e: unknown) => console.error('Explain plainly failed', e));
        },
      }),
      registerSettingsPane({ id: 'actions', title: 'Actions', order: 62, render: () => <ActionsPane api={api} /> }),
    ];
    return () => offs.forEach((off) => off());
  }, [shell]);

  // Search ranks chats the user opens often a little higher: count each chat the selection lands on.
  useEffect(() => {
    let last: string | null = null;
    return shell.subscribe(() => {
      const id = shell.selection.chatId;
      if (id && id !== last) void searchOps(shell.api).recordOpen(id).catch(() => undefined);
      last = id;
    });
  }, [shell]);
  return null;
}
