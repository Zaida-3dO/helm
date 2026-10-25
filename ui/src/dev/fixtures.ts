// Builders for records in tests and in the dev fake host.

import type { ChatRecord, MessageRow, Submission, ThreadState } from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';

let nextId = 1;

export function row(p: Partial<MessageRow> & Pick<MessageRow, 'role'>): MessageRow {
  const id = p.id ?? nextId++;
  return {
    id,
    chatId: 'c1',
    sourceKey: `k${id}`,
    text: '',
    thinking: '',
    thinkingTail: null,
    toolName: null,
    toolCallId: null,
    status: 'complete',
    timestamp: id * 1000,
    sequence: id,
    revision: 1,
    artifact: null,
    details: {},
    ...p,
  };
}

export function chat(p: Partial<ChatRecord> = {}): ChatRecord {
  return {
    id: 'c1',
    projectId: 'p1',
    cwd: 'C:/work',
    title: 'New chat',
    status: 'idle',
    error: null,
    harness: 'claude',
    model: null,
    effort: null,
    draft: '',
    draftRevision: 0,
    createdAt: 0,
    updatedAt: 0,
    revision: 1,
    pinned: false,
    archived: false,
    parentChatId: null,
    sideSourceId: null,
    titleState: 'fallback',
    unread: { markToken: null, completionId: null },
    muted: false,
    lastActivityAt: 0,
    stalled: false,
    ...p,
  };
}

export function thread(p: Partial<ThreadState> = {}): ThreadState {
  return {
    chatId: 'c1',
    status: 'idle',
    permissionMode: null,
    interactions: [],
    liveTools: [],
    backgroundTasks: [],
    subagents: [],
    subagentsRunning: 0,
    compacting: null,
    capabilities: { withdraw: true, deliverNow: true, stopTool: false, compact: false },
    sendBehavior: { ...HOME_SEND_BEHAVIOR },
    revision: 1,
    ...p,
  };
}

export function submission(p: Partial<Submission> & Pick<Submission, 'id'>): Submission {
  return {
    chatId: 'c1',
    text: 'hello',
    mode: 'followUp',
    state: 'queued',
    origin: 'typed',
    position: null,
    createdAt: 0,
    ...p,
  };
}
