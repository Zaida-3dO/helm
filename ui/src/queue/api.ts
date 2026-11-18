// Typed wrappers over the queue ops. Every call goes through the shared api(op, args).

import type { Api } from '../api/client.ts';
import type { QueueSummary } from '../api/generated/index.ts';

export type QueueAction = 'top' | 'up' | 'down' | 'hold' | 'release' | 'cancel';

export function queueOps(api: Api) {
  return {
    /** Every waiting turn start, front of the lines first, plus what each place carries. */
    list: () => api<QueueSummary>('queue_list', {}),
    /** One move on one waiting start; the answer is the fresh queue. */
    act: (chatId: string, action: QueueAction) => api<QueueSummary>('queue_action', { chatId, action }),
  };
}

export type QueueOps = ReturnType<typeof queueOps>;
