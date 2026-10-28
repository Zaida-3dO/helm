// The GitHub ops (host/crates/github) that Settings → GitHub uses. The pull request view has its
// own calls; these are the switch and "Check sign-in".

import type { Api } from '../../api/client.ts';

/** The pull request view switch. `revision` guards saves: a stale one is refused. */
export interface GitHubSettings {
  enabled: boolean;
  revision: number;
}

export interface RateLimit {
  limit: number;
  remaining: number;
  used: number;
  /** Milliseconds since the epoch. */
  resetAt: number;
}

/** Every GitHub answer. A failure is an error whose message is `[Class] sentence`. */
export interface GitHubAnswer<T> {
  status: number;
  rateLimit: RateLimit | null;
  nextPage?: number;
  data: T;
  errors?: unknown;
}

export interface Viewer {
  login: string;
  name: string | null;
  avatarUrl: string;
}

export function githubOps(api: Api) {
  return {
    settings: () => api<GitHubSettings>('github_settings_get', {}),
    save: (enabled: boolean, expectedRevision: number) => api<GitHubSettings>('github_settings_save', { enabled, expectedRevision }),
    viewer: () => api<GitHubAnswer<Viewer>>('github_viewer', {}),
  };
}

/** `[Class] sentence` → its parts; a message without a class is all sentence. */
export function splitError(message: string): { cls: string | null; text: string } {
  const m = /^\[([A-Za-z]+)\] (.*)$/s.exec(message);
  return m ? { cls: m[1] ?? null, text: m[2] ?? '' } : { cls: null, text: message };
}
