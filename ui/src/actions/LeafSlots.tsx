// Registers the actions area's slot contributions with the other areas' registries: quick
// replies above the composer, option buttons under the latest reply, Explain plainly in a reply's
// actions, and the Actions pane in Settings. Mounted once in the app frame; it draws nothing itself.

import { useEffect } from 'react';
import { searchOps } from '../api/search.ts';
import { ActionsPane } from '../settings/actions/ActionsPane.tsx';
import { registerComposerAccessory } from '../composer/slots.ts';
import { registerSettingsPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';
import { savedFromText, savedOps } from '../saved/model.ts';
import { registerMessageAction, registerRowFooter, registerSelectionAction } from '../transcript/slots.ts';
import { toast } from '../ui/index.ts';
import { IconBookmark } from '../ui/icons.tsx';
import { explainAppliesTo, ExplainPlainlyIcon } from './ExplainPlainly.tsx';
import { AnswerButton } from './AnswerPopup.tsx';
import { OptionButtons, QuickReplies } from './Replies.tsx';
import { EXPLAIN_PLAINLY_TEXT, sendActionText } from './send.ts';

export function LeafSlots(): null {
  const shell = useShell();
  useEffect(() => {
    const api = shell.api;
    const offs = [
      registerComposerAccessory({
        id: 'leaf-answer-button',
        placement: 'above',
        render: (ctx) => <AnswerButton chatId={ctx.chat.id} />,
      }),
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
        icon: ExplainPlainlyIcon,
        appliesTo: explainAppliesTo,
        run: (_row, ctx) => {
          sendActionText(api, ctx.chatId, EXPLAIN_PLAINLY_TEXT).catch((e: unknown) => console.error('Explain plainly failed', e));
        },
      }),
      registerMessageAction({
        id: 'leaf-save-for-later',
        label: 'Save for later',
        icon: IconBookmark,
        order: 20,
        appliesTo: (row) => (row.role === 'assistant' || row.role === 'user') && row.text.trim() !== '',
        run: (row, ctx) => {
          const { title, body } = savedFromText(row.text);
          savedOps(api)
            .save({ chatId: ctx.chatId, messageId: row.id, title, body })
            .then(() => toast.show({ kind: 'info', text: 'Saved for later — open Settings → Saved to see it.' }))
            .catch((e: unknown) => toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` }));
        },
      }),
      registerSelectionAction({
        id: 'leaf-save-selection',
        label: 'Save for later',
        appliesTo: (sel) => sel.text.trim() !== '',
        run: (sel) => {
          const { title, body } = savedFromText(sel.text);
          savedOps(api)
            .save({ chatId: sel.chatId, title, body })
            .then(() => toast.show({ kind: 'info', text: 'Saved for later — open Settings → Saved to see it.' }))
            .catch((e: unknown) => toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` }));
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
