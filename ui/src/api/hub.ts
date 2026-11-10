// The hub line, its Telegram chat and its voice chat (host/crates/relay), for Settings → Hub.

import type { Api } from './client.ts';

/** `off`: not set up (no address or no token). */
export type LineState = 'off' | 'connecting' | 'online' | 'offline';

/** What Settings may see of the connection: whether each part is set, never a value. */
export interface HubConnection {
  hasAddress: boolean;
  hasFallback: boolean;
  hasToken: boolean;
  state: LineState;
  /** Who this PC is paired as, once it has paired with a code. */
  personName?: string | null;
  machineName?: string | null;
}

/** What pairing with a code answered: who this PC now is on the hub. */
export interface HubPairResult {
  connection: HubConnection;
  personId: string;
  personName: string;
  machineId: string;
  machineName: string;
  memoryServer: string;
  profileId: string;
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
  /** The model a new Telegram chat starts with; `null`: the same as any other new chat. */
  model: string | null;
  /** The effort a new Telegram chat starts with; `null`: the same as any other new chat. */
  effort: string | null;
}

/** What the attention watch wakes the voice chat for. */
export type VoiceWatch = 'off' | 'flags' | 'flags+done';

/** The chat the phone's voice screen talks to while its tag holds. */
export interface VoiceChat {
  id: string;
  title: string;
  startedAt: number;
  /** Tokens used since it became the voice chat (what the token limit counts). */
  tokensUsed: number;
}

export interface VoiceSettings {
  /** `null`: the next voice message starts a new chat. */
  chat: VoiceChat | null;
  /** The model a new voice chat starts with; blank: the same as any other new chat. */
  model: string;
  /** The effort a new voice chat starts with; blank: the same as any other new chat. */
  effort: string;
  /** A message arriving this long after the last one starts a new chat. */
  idleMinutes: number;
  maxAgeHours: number;
  maxTokens: number;
  watch: VoiceWatch;
}

export function hubOps(api: Api) {
  return {
    connection: () => api<HubConnection>('relay_connection_get', {}),
    /** Trade a one-time code for this PC's token; sets up the line and the memory server. */
    pair: (a: { address: string; fallback?: string; code: string }) => api<HubPairResult>('hub_pair', a),
    saveConnection: (a: HubConnectionSave) => api<HubConnection>('relay_connection_save', a),
    telegram: () => api<TelegramSettings>('relay_telegram_get', {}),
    saveTelegram: (
      s: Pick<TelegramSettings, 'maxAgeHours' | 'maxTokens' | 'awayAlerts' | 'approvals' | 'model' | 'effort'>,
    ) =>
      api<TelegramSettings>('relay_telegram_save', {
        maxAgeHours: s.maxAgeHours,
        maxTokens: s.maxTokens,
        awayAlerts: s.awayAlerts,
        approvals: s.approvals,
        model: s.model,
        effort: s.effort,
      }),
    resetTelegram: () => api<TelegramSettings>('relay_telegram_reset', {}),
    voice: () => api<VoiceSettings>('relay_voice_get', {}),
    saveVoice: (s: Omit<VoiceSettings, 'chat'>) =>
      api<VoiceSettings>('relay_voice_save', {
        model: s.model,
        effort: s.effort,
        idleMinutes: s.idleMinutes,
        maxAgeHours: s.maxAgeHours,
        maxTokens: s.maxTokens,
        watch: s.watch,
      }),
    resetVoice: () => api<VoiceSettings>('relay_voice_reset', {}),
  };
}
