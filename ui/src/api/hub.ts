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
}

export function hubOps(api: Api) {
  return {
    connection: () => api<HubConnection>('relay_connection_get', {}),
    saveConnection: (a: HubConnectionSave) => api<HubConnection>('relay_connection_save', a),
    telegram: () => api<TelegramSettings>('relay_telegram_get', {}),
    saveTelegram: (maxAgeHours: number, maxTokens: number) => api<TelegramSettings>('relay_telegram_save', { maxAgeHours, maxTokens }),
    resetTelegram: () => api<TelegramSettings>('relay_telegram_reset', {}),
  };
}
