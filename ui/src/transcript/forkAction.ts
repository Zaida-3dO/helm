// "Fork from here" on assistant replies: forks the chat at that reply and
// opens the fork. Registered through the transcript slots by the chat view.

import type { ChatOps } from '../api/ops.ts';
import type { MessageRow } from '../api/types.ts';
import type { MessageAction } from './slots.ts';

export const FORK_ACTION_ID = 'fork-from-here';

/** Only complete assistant replies are fork points. */
export function canForkAt(row: MessageRow): boolean {
  return row.role === 'assistant' && row.status === 'complete';
}

export function forkAction(ops: ChatOps, open: (chatId: string) => void, onError: (message: string) => void): MessageAction {
  return {
    id: FORK_ACTION_ID,
    label: 'Fork from here',
    appliesTo: canForkAt,
    run: (row, ctx) => {
      ops
        .fork(ctx.chatId, { messageId: row.id })
        .then((c) => open(c.id))
        .catch((e: unknown) => onError(e instanceof Error ? e.message : String(e)));
    },
  };
}
