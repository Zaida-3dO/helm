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
