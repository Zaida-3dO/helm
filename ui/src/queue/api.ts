// Typed wrappers over the queue ops. Every call goes through the shared api(op, args).
// The build queue ops answer every call with the fresh list, as the chat queue does.

import type { Api } from '../api/client.ts';
import type { BuildQueueAddArgs, BuildQueueEntry, QueueSummary } from '../api/generated/index.ts';

export type QueueAction = 'top' | 'up' | 'down' | 'move' | 'hold' | 'release' | 'cancel';

export function queueOps(api: Api) {
  return {
    /** Every waiting turn start, front of the lines first, plus what each place carries. */
    list: () => api<QueueSummary>('queue_list', {}),
    /**
     * One move on one waiting start; the answer is the fresh queue. `move` names the
     * waiting chat to stand in front of (`beforeChatId`; none means the end of the line).
     */
    act: (chatId: string, action: QueueAction, beforeChatId?: string | null) =>
      api<QueueSummary>('queue_action', { chatId, action, ...(beforeChatId ? { beforeChatId } : {}) }),
  };
}

export type QueueOps = ReturnType<typeof queueOps>;

export function buildQueueOps(api: Api) {
  return {
    /** The features queued to run one after another, front of the line first; removed ones are gone. */
    list: () => api<BuildQueueEntry[]>('build_queue_list', {}),
    /** Join the back of the line: a title plus a brief or a board item; project and model optional. */
    add: (args: BuildQueueAddArgs) => api<BuildQueueEntry[]>('build_queue_add', args),
    /** Stand this entry just before another (`beforeId` omitted is the back of the line). */
    move: (id: string, beforeId?: string | null) =>
      api<BuildQueueEntry[]>('build_queue_move', { id, ...(beforeId ? { beforeId } : {}) }),
    /** Stop tracking an entry for a while; its place and its started chat are kept. */
    pause: (id: string) => api<BuildQueueEntry[]>('build_queue_pause', { id }),
    /** Put it back in line: waiting again, or tracked again when its chat had started. */
    resume: (id: string) => api<BuildQueueEntry[]>('build_queue_resume', { id }),
    /** Take it out of the list for good; a chat it already started keeps running. */
    remove: (id: string) => api<BuildQueueEntry[]>('build_queue_remove', { id }),
  };
}

export type BuildQueueOps = ReturnType<typeof buildQueueOps>;
