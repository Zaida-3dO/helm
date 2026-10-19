// Typed wrappers over the workflow ops (host/crates/workflow, ops.rs). Every call still goes
// through api(op, args). The shapes are the generated bindings; nothing here is hand-copied.

import type { Api } from '../api/client.ts';
import type { ChatMode, ChatModeSetResult, ChatModeView, WorkflowSettings } from '../api/generated/workflow/index.ts';

export type {
  ChatMode,
  ChatModeSetResult,
  ChatModeView,
  EditsRule,
  LandMode,
  Landing,
  MergeMethod,
  NewChats,
  RepoRule,
  SetBy,
  SignOff,
  StageUse,
  WorkflowSettings,
} from '../api/generated/workflow/index.ts';

/** The user's buttons on one change (host `workflow_change_action`). */
export type ChangeAction = 'merge' | 'decline' | 'drop' | 'retry' | 'ready';
/** The user's Sign off plan and task menu (host `workflow_task_action`). */
export type TaskAction = 'signOff' | 'skip' | 'allowMore' | 'park' | 'reopen' | 'visual' | 'notVisual' | 'end';
export interface TaskActionFields {
  planId?: string;
  stage?: string;
  n?: number;
}

export function workflowOps(api: Api) {
  return {
    settingsGet: () => api<WorkflowSettings>('workflow_settings_get', {}),
    settingsSave: (settings: WorkflowSettings) => api<WorkflowSettings>('workflow_settings_save', { settings }),
    modeGet: (chatId: string) => api<ChatModeView>('workflow_chat_mode_get', { chatId }),
    modeSet: (chatId: string, mode: ChatMode) => api<ChatModeSetResult>('workflow_chat_mode_set', { chatId, mode }),
    changeAction: (runId: string, repo: string, branch: string, action: ChangeAction) =>
      api<{ state: string }>('workflow_change_action', { runId, repo, branch, action }),
    taskAction: (runId: string, action: TaskAction, fields: TaskActionFields = {}) =>
      api<Record<string, unknown>>('workflow_task_action', { runId, action, ...fields }),
  };
}

export type WorkflowOps = ReturnType<typeof workflowOps>;
