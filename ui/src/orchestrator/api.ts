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

export function workflowOps(api: Api) {
  return {
    settingsGet: () => api<WorkflowSettings>('workflow_settings_get', {}),
    settingsSave: (settings: WorkflowSettings) => api<WorkflowSettings>('workflow_settings_save', { settings }),
    modeGet: (chatId: string) => api<ChatModeView>('workflow_chat_mode_get', { chatId }),
    modeSet: (chatId: string, mode: ChatMode) => api<ChatModeSetResult>('workflow_chat_mode_set', { chatId, mode }),
  };
}

export type WorkflowOps = ReturnType<typeof workflowOps>;
