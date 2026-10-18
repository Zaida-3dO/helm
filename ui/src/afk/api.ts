// Typed wrappers over the AFK ops (host crate helm-afk). Hand-written: the shapes are
// small and the host serialises them with serde's camelCase.

import type { Api } from '../api/client.ts';

export type AfkSource = 'manual' | 'screenLock' | 'sleep' | 'idle' | 'unanswered';
export type AutoEnd = 'onReturn' | 'onReturnAutoOnly' | 'manual';

export interface AfkSettings {
  enabled: boolean;
  appliesTo: 'all' | 'chat';
  answer: string | null;
  planApproval: 'defer' | 'approve';
  digestPrompt: string | null;
  auto: {
    screenLock: boolean;
    lockGraceSeconds: number;
    sleep: boolean;
    unansweredMinutes: number | null;
    idleMinutes: number | null;
    end: AutoEnd;
  };
}

export interface AfkSpan {
  id: string;
  startedAt: number;
  endedAt: number | null;
  source: AfkSource;
}

export interface AfkDigest {
  chatId: string;
  title: string;
  state: 'pending' | 'queued' | 'sent' | 'failed' | 'dismissed';
  attempts: number;
  error: string | null;
}

export interface AfkSpanDigests {
  span: AfkSpan;
  digests: AfkDigest[];
}

export interface AfkView {
  settings: AfkSettings;
  open: AfkSpan | null;
  seq: number;
  last: AfkSpanDigests | null;
  failures: AfkSpanDigests | null;
  used: boolean;
  defaults: { settings: AfkSettings; digestPrompt: string };
}

export function afkOps(api: Api) {
  return {
    state: () => api<AfkView>('afk_state', {}),
    save: (settings: AfkSettings) => api<AfkView>('afk_settings', { settings }),
    set: (on: boolean) => api<AfkView>('afk_set', { on }),
    retry: (spanId: string) => api<AfkView>('afk_retry', { spanId }),
    dismiss: (spanId: string) => api<AfkView>('afk_dismiss_failures', { spanId }),
    presence: (p: { focused?: boolean; input?: boolean }) => api<null>('afk_presence', p),
  };
}
