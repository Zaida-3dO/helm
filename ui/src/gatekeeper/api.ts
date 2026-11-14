// Typed wrappers over the gatekeeper's ops. Every call goes through the shared api(op, args).

import type { Api } from '../api/client.ts';
import type { GateAsk, GateSettingsEdit, GateSettingsView } from './types.ts';

export interface GateListQuery {
  filter?: string;
  query?: string;
  limit?: number;
}

export function gateOps(api: Api) {
  return {
    list: (q: GateListQuery = {}) => api<{ rows: GateAsk[] }>('gate_list', q).then((r) => r.rows),
    review: (id: string, review: string, note?: string) => api<GateAsk>('gate_review', { id, review, ...(note ? { note } : {}) }),
    changeAnswer: (id: string, answer: string) => api<GateAsk>('gate_change_answer', { id, answer }),
    settingsGet: () => api<GateSettingsView>('gate_settings_get', {}),
    settingsSave: (settings: GateSettingsEdit) => api<GateSettingsView>('gate_settings_save', { settings }),
  };
}

export type GateOps = ReturnType<typeof gateOps>;
