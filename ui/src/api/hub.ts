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
  /** Set when a saved connection exists but could not be read: the reason, in words. */
  configProblem?: string;
  /** Why the last attempt to reach the hub failed; absent while connected. */
  lineRefusal?: string;
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
  /** Waiting prompts can be answered from the hub's Remote page. */
  remoteAnswers: boolean;
  /** The model a new Telegram chat starts with; `null`: the same as any other new chat. */
  model: string | null;
  /** The effort a new Telegram chat starts with; `null`: the same as any other new chat. */
  effort: string | null;
  /** Whether this PC takes this profile's person's Telegram (default: on for the home profile). */
  takeTelegram: boolean;
}

/** One profile's Telegram account on the hub, as the link row shows it. */
export interface TelegramLink {
  linked: boolean;
  /** The bot's handle, e.g. `@myhelm` — set when linked and the hub knows the bot's name. */
  handle?: string | null;
}

/** What `relay_tg_link` answered: the one-tap address and when its code stops working. */
export interface TelegramLinkOffer {
  url: string;
  /** Epoch milliseconds; the code stops working then. */
  expiresAt: number;
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
  /** Whether this PC takes this profile's person's voice (default: on for the home profile). */
  takeVoice: boolean;
}

/** One channel's prompt for one profile, as the host holds it. */
export interface ChannelPrompt {
  channel: 'speaker' | 'household' | 'voice' | 'telegram';
  label: string;
  /** What applies when nothing is saved. */
  default: string;
  /** What a new conversation is told now: the saved text, else the default. */
  text: string;
  /** Whether a saved replacement is in force. */
  custom: boolean;
}

export interface ChannelPrompts {
  channels: ChannelPrompt[];
  maxChars: number;
}

/** The profile a channel op acts on; absent: the home profile (the host's own default). */
export interface ChannelScope {
  profileId?: string | null;
}

function scope(s: ChannelScope): { profileId?: string } {
  return s.profileId ? { profileId: s.profileId } : {};
}

export function hubOps(api: Api) {
  return {
    connection: () => api<HubConnection>('relay_connection_get', {}),
    /** Trade a one-time code for this PC's token; sets up the line and the memory server. */
    pair: (a: { address: string; fallback?: string; code: string }) => api<HubPairResult>('hub_pair', a),
    saveConnection: (a: HubConnectionSave) => api<HubConnection>('relay_connection_save', a),
    telegram: (s: ChannelScope = {}) => api<TelegramSettings>('relay_telegram_get', scope(s)),
    saveTelegram: (
      s: Pick<TelegramSettings, 'maxAgeHours' | 'maxTokens' | 'awayAlerts' | 'approvals' | 'remoteAnswers' | 'model' | 'effort' | 'takeTelegram'> & ChannelScope,
    ) =>
      api<TelegramSettings>('relay_telegram_save', {
        maxAgeHours: s.maxAgeHours,
        maxTokens: s.maxTokens,
        awayAlerts: s.awayAlerts,
        approvals: s.approvals,
        remoteAnswers: s.remoteAnswers,
        model: s.model,
        effort: s.effort,
        takeTelegram: s.takeTelegram,
        ...scope(s),
      }),
    resetTelegram: (s: ChannelScope = {}) => api<TelegramSettings>('relay_telegram_reset', scope(s)),
    voice: (s: ChannelScope = {}) => api<VoiceSettings>('relay_voice_get', scope(s)),
    saveVoice: (s: Omit<VoiceSettings, 'chat'> & ChannelScope) =>
      api<VoiceSettings>('relay_voice_save', {
        model: s.model,
        effort: s.effort,
        idleMinutes: s.idleMinutes,
        maxAgeHours: s.maxAgeHours,
        maxTokens: s.maxTokens,
        watch: s.watch,
        takeVoice: s.takeVoice,
        ...scope(s),
      }),
    resetVoice: (s: ChannelScope = {}) => api<VoiceSettings>('relay_voice_reset', scope(s)),
    prompts: (s: ChannelScope = {}) => api<ChannelPrompts>('relay_prompts_get', scope(s)),
    /** Replace one channel's prompt; blank puts the default back. Applies to new conversations. */
    savePrompt: (s: ChannelScope & { channel: ChannelPrompt['channel']; text: string }) =>
      api<ChannelPrompts>('relay_prompts_save', { ...scope(s), channel: s.channel, text: s.text }),
    /** Ask the hub for a one-tap address that links this profile's person's Telegram account. */
    tgLink: (s: ChannelScope = {}) => api<TelegramLinkOffer>('relay_tg_link', scope(s)),
    /** Whether the profile's person already has a linked Telegram account, and the bot's handle. */
    tgStatus: (s: ChannelScope = {}) => api<TelegramLink>('relay_tg_status', scope(s)),
    /** Drop the profile's person's Telegram link from the hub. */
    tgUnlink: (s: ChannelScope = {}) => api<{ unlinked: number }>('relay_tg_unlink', scope(s)),
    /** Ask the hub to send this profile's person's channel to this PC ("Use this PC"). */
    routePin: (s: ChannelScope & { channel: 'telegram' | 'voice' }) => api<{ pinned: boolean }>('relay_route_pin', { ...scope(s), channel: s.channel }),
  };
}
