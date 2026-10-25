// The hub line and its Telegram chat (host/crates/relay), for Settings → Hub.

import type { Api } from './client.ts';

/** `off`: not set up (no address or no token). */
export type LineState = 'off' | 'connecting' | 'online' | 'offline';

/** What Settings may see of the connection: whether each part is set, never a value. */
export interface HubConnection {
  hasAddress: boolean;
  hasFallback: boolean;
  hasToken: boolean;
  state: LineState;
}

/** Each field absent keeps the stored value; an empty string clears it. */
export interface HubConnectionSave {
  lanUrl?: string;
  fallbackUrl?: string;
  token?: string;
}

/** The chat Telegram messages go to while its tag holds. */
export interface TelegramChat {
  id: string;
  title: string;
  startedAt: number;
  /** Tokens used since it became the Telegram chat (what the token limit counts). */
  tokensUsed: number;
}

export interface TelegramSettings {
  /** `null`: the next Telegram message starts a new chat. */
  chat: TelegramChat | null;
  maxAgeHours: number;
  maxTokens: number;
  /** While the user is away, a chat that newly needs them sends one line on Telegram. */
  awayAlerts: boolean;
  /** Permission prompts and plan approvals can be answered with Telegram buttons. */
  approvals: boolean;
}

export function hubOps(api: Api) {
  return {
    connection: () => api<HubConnection>('relay_connection_get', {}),
    saveConnection: (a: HubConnectionSave) => api<HubConnection>('relay_connection_save', a),
    telegram: () => api<TelegramSettings>('relay_telegram_get', {}),
    saveTelegram: (s: Pick<TelegramSettings, 'maxAgeHours' | 'maxTokens' | 'awayAlerts' | 'approvals'>) =>
      api<TelegramSettings>('relay_telegram_save', {
        maxAgeHours: s.maxAgeHours,
        maxTokens: s.maxTokens,
        awayAlerts: s.awayAlerts,
        approvals: s.approvals,
      }),
    resetTelegram: () => api<TelegramSettings>('relay_telegram_reset', {}),
  };
}
