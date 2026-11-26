// The local model ops (host/crates/localmodels) that Settings → Local models uses. A server's key is
// write-only: no op returns it, only whether one is set.

import type { Api } from '../../api/client.ts';
import type { LocalModelsState, RefreshResult, TestResult } from './model.ts';
import { savePayload, type ServerDraft } from './model.ts';

export interface SettingsPatch {
  ollamaContext?: number;
  keepAliveMinutes?: number;
  strataNeedGb?: number;
  tools?: LocalModelsState['tools'];
  compareWith?: LocalModelsState['compareWith'];
  games?: string[];
  /** Model id to the name the user gives it; an empty string clears that model's name. */
  aliases?: Record<string, string>;
  /** Model id to the chat size it works well up to, in tokens; 0 clears one and falls back to 70% of its window. */
  comfort?: Record<string, number>;
  /** Model id to another local model id suggested when a chat outgrows it; an empty string clears one. */
  stepUp?: Record<string, string>;
  gameLiftMinutes?: number;
  /** An empty string clears the task or the file. */
  restartTask?: string;
  pauseFile?: string;
}

export function localModelsOps(api: Api) {
  return {
    state: () => api<LocalModelsState>('local_models_state', {}),
    /** Saves the whole server list (it replaces the list). A refusal comes back as an error with a plain message. */
    saveServers: (drafts: readonly ServerDraft[]) => api<LocalModelsState>('local_models_save', { servers: savePayload(drafts) }),
    saveSettings: (patch: SettingsPatch) => api<LocalModelsState>('local_models_save', patch),
    setKey: (serverId: string, key: string) => api<LocalModelsState>('local_server_key_set', { serverId, key }),
    forgetKey: (serverId: string) => api<LocalModelsState>('local_server_key_forget', { serverId }),
    test: (serverId: string) => api<TestResult>('local_server_test', { serverId }),
    /** Asks every enabled server what it has. The only thing here that contacts them besides Test. */
    refresh: () => api<RefreshResult>('local_models_refresh', {}),
  };
}
