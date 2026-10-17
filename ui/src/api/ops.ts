// Typed wrappers over the chat ops. Every call still goes through api(op, args).

import type { Api } from './client.ts';
import type {
  ChatRecord,
  ModelCatalogue,
  SendAction,
  Submission,
  SubmissionAction,
  ThreadState,
  TranscriptPage,
} from './types.ts';

export interface PageQuery {
  before?: string;
  after?: string;
  around?: number;
  inclusive?: boolean;
}

export interface InteractionAnswer {
  value?: string;
  confirmed?: boolean;
  cancelled?: boolean;
  goto?: number;
}

export function chatOps(api: Api) {
  return {
    create: (cwd: string, model?: string, effort?: string) => api<ChatRecord>('chat_create', { cwd, model, effort }),
    list: () => api<ChatRecord[]>('chat_list', {}),
    get: (chatId: string) => api<{ chat: ChatRecord; thread: ThreadState }>('chat_get', { chatId }),
    page: (chatId: string, q: PageQuery = {}) => api<TranscriptPage>('transcript_page', { chatId, ...q }),
    send: (chatId: string, submissionId: string, text: string, action: SendAction, draftRevision: number) =>
      api<{ submission: Submission; chat: ChatRecord }>('chat_send', {
        chatId,
        submissionId,
        text,
        action,
        draftRevision,
      }),
    stop: (chatId: string, continueQueue: boolean) => api<{ status: string }>('chat_stop', { chatId, continueQueue }),
    resumeQueue: (chatId: string) => api<null>('queue_resume', { chatId }),
    submissionAction: (submissionId: string, action: SubmissionAction, position?: number) =>
      api<{ submission: Submission }>('submission_action', { submissionId, action, position }),
    answer: (chatId: string, requestId: string, a: InteractionAnswer) =>
      api<null>('interaction_answer', { chatId, requestId, ...a }),
    saveDraft: (chatId: string, text: string, revision: number) =>
      api<{ revision: number }>('draft_save', { chatId, text, revision }),
    setModel: (chatId: string, model?: string, effort?: string) =>
      api<ChatRecord>('chat_set_model', { chatId, model, effort }),
    catalogue: (refresh = false) => api<ModelCatalogue>('model_catalogue', { harness: 'claude', refresh }),
    stopTool: (chatId: string, toolCallId: string) => api<{ stopped: boolean }>('tool_stop', { chatId, toolCallId }),
    compact: (chatId: string) => api<null>('chat_compact', { chatId }),
  };
}

export type ChatOps = ReturnType<typeof chatOps>;
