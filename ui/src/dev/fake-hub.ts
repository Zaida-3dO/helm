// The dev fake host's hub ops (Settings → Hub in a plain browser): a connection that goes
// "connecting" then "connected" once an address and token are saved, and a Telegram chat and a
// voice chat with the default limits. Pairing with the code ABCD2345 works. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { HubConnection, SpeakerSettings, TelegramSettings, VoiceSettings } from '../api/hub.ts';
import type { FakeOpHandler } from './fakeHost.ts';

/** `?household=` in the page URL starts the fake already paired to a hub (see fake-profiles.ts). */
export function pairedAtStart(): boolean {
  return typeof location !== 'undefined' && new URLSearchParams(location.search).has('household');
}

export function hubFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const set = { lanUrl: pairedAtStart(), fallbackUrl: false, token: pairedAtStart() };
  let who: { personName: string; machineName: string } | null = pairedAtStart() ? { personName: 'Alex', machineName: 'Desk PC' } : null;
  let since = 0;
  // The launcher: on by default once paired, like the host; its process and logon entry follow.
  // The sign-in switch is on by default, and choosing it by hand brings the launcher with it.
  const launcher: { enabled?: boolean; startAtSignIn?: boolean } = {};
  // Per-profile policy and link state, keyed as the host keys it (absent profile: "home").
  const scope = (a: Record<string, unknown>) => (typeof a.profileId === 'string' && a.profileId ? a.profileId : 'home');
  interface ProfileState {
    tg: TelegramSettings;
    voice: VoiceSettings;
    speaker: SpeakerSettings;
    linked: boolean;
    pinned: string[];
    prompts: Record<string, string>;
  }
  const profiles = new Map<string, ProfileState>();
  const state = (id: string): ProfileState => {
    let p = profiles.get(id);
    if (!p) {
      p = {
        tg: { chat: null, maxAgeHours: 24, maxTokens: 400_000, awayAlerts: true, approvals: false, remoteAnswers: false, model: null, effort: null, takeTelegram: id === 'home' },
        voice: { chat: null, model: 'claude-sonnet-5-5', effort: 'low', idleMinutes: 240, maxAgeHours: 24, maxTokens: 300_000, watch: 'flags', takeVoice: id === 'home', quickModel: '', quickEffort: '', quickRollAt: 0, deepRollAt: 0 },
        speaker: { talks: [], model: 'local/strata/qwen3.8-flash-next-iq3_s', effort: 'low', takeSpeaker: id === 'home', previousHours: 12, lean: false, leanPrompt: '', leanPromptDefault: 'You are Helm, the voice of this house, answering out loud through a room speaker.', leanTools: ['home assistant', 'memory_search', 'app_schedule', 'app_list_chats', 'app_read_chat', 'app_telegram_send'], leanToolsDefault: ['home assistant', 'memory_search', 'app_schedule', 'app_list_chats', 'app_read_chat', 'app_telegram_send'], leanToolsCustom: false, boardId: '', quickModel: '', quickEffort: '', quickRollAt: 0, deepRollAt: 0, kept: [] },
        linked: false,
        pinned: [],
        prompts: {},
      };
      profiles.set(id, p);
    }
    return p;
  };
  const view = (): HubConnection => ({
    hasAddress: set.lanUrl,
    hasFallback: set.fallbackUrl,
    hasToken: set.token,
    state: !(set.lanUrl || set.fallbackUrl) || !set.token ? 'off' : Date.now() - since < 3000 ? 'connecting' : 'online',
    personName: who?.personName ?? null,
    machineName: who?.machineName ?? null,
  });
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'relay_connection_get':
        return view();
      case 'relay_reconnect':
        // Line up again right away: "connecting" for three seconds, then connected.
        if (!(set.lanUrl || set.fallbackUrl) || !set.token) throw new ApiError('refused', 'The hub line is not set up yet.');
        since = Date.now();
        return view();
      case 'launcher_get':
      case 'launcher_set': {
        if (op === 'launcher_set') {
          if (typeof a.enabled === 'boolean') launcher.enabled = a.enabled;
          if (typeof a.startAtSignIn === 'boolean') launcher.startAtSignIn = a.startAtSignIn;
          if (typeof a.enabled !== 'boolean' && typeof a.startAtSignIn !== 'boolean')
            throw new ApiError('bad_args', 'Expected enabled or startAtSignIn to be true or false.');
        }
        const paired = !!who && (set.lanUrl || set.fallbackUrl) && set.token;
        const on = launcher.enabled ?? (paired || launcher.startAtSignIn === true);
        return {
          enabled: on,
          startAtSignIn: launcher.startAtSignIn ?? true,
          running: on && paired,
          registered: on && paired,
          wakeReady: on && paired,
        };
      }
      case 'relay_connection_save':
        for (const k of ['lanUrl', 'fallbackUrl', 'token'] as const) {
          const v = a[k];
          if (typeof v !== 'string') continue;
          if (k !== 'token' && v.trim() && !/^(ws|http):\/\//.test(v.trim())) throw new ApiError('bad_args', 'The hub address must start with ws:// or http://.');
          set[k] = v.trim() !== '';
        }
        since = Date.now();
        return view();
      case 'hub_pair': {
        const address = String(a.address ?? '').trim();
        if (!/^(ws|http):\/\//.test(address)) throw new ApiError('bad_args', 'The hub address must start with ws:// or http://.');
        if (String(a.code ?? '').trim().toUpperCase() !== 'ABCD2345') throw new ApiError('bad_args', 'That pairing code is not valid, or it has run out. Ask for a new one.');
        set.lanUrl = true;
        set.fallbackUrl = typeof a.fallback === 'string' && a.fallback.trim() !== '';
        set.token = true;
        since = Date.now();
        who = { personName: 'Alex', machineName: 'Desk PC' };
        ctx.emit({ kind: 'profiles_changed' });
        return { connection: view(), personId: 'p-self', personName: who.personName, machineId: 'm1', machineName: who.machineName, memoryServer: 'hub-memory', profileId: 'prof-home' };
      }
      case 'relay_telegram_get': {
        const st = state(scope(a));
        if (!st.tg.chat) {
          const first = (ctx.call('chat_list', {}) as { id: string; title: string }[])[0];
          if (first) st.tg.chat = { id: first.id, title: first.title, startedAt: Date.now() - 3_600_000, tokensUsed: 12_400 };
        }
        return st.tg;
      }
      case 'relay_telegram_save': {
        const st = state(scope(a));
        st.tg.maxAgeHours = Number(a.maxAgeHours);
        st.tg.maxTokens = Number(a.maxTokens);
        st.tg.awayAlerts = a.awayAlerts !== false;
        st.tg.approvals = a.approvals === true;
        st.tg.remoteAnswers = a.remoteAnswers === true;
        // Blank or missing counts as unset, as the host reads it.
        st.tg.model = typeof a.model === 'string' && a.model.trim() ? a.model.trim() : null;
        st.tg.effort = typeof a.effort === 'string' && a.effort.trim() ? a.effort.trim() : null;
        if (typeof a.takeTelegram === 'boolean') st.tg.takeTelegram = a.takeTelegram;
        return st.tg;
      }
      case 'relay_telegram_reset': {
        const st = state(scope(a));
        st.tg.chat = null;
        return st.tg;
      }
      case 'relay_voice_get':
        return state(scope(a)).voice;
      case 'relay_voice_save': {
        const v = state(scope(a)).voice;
        v.model = typeof a.model === 'string' ? a.model.trim() : '';
        v.effort = typeof a.effort === 'string' ? a.effort.trim() : '';
        v.idleMinutes = Number(a.idleMinutes);
        v.maxAgeHours = Number(a.maxAgeHours);
        v.maxTokens = Number(a.maxTokens);
        v.watch = a.watch === 'off' || a.watch === 'flags+done' ? a.watch : 'flags';
        if (typeof a.takeVoice === 'boolean') v.takeVoice = a.takeVoice;
        v.quickModel = typeof a.quickModel === 'string' ? a.quickModel.trim() : '';
        v.quickEffort = typeof a.quickEffort === 'string' ? a.quickEffort.trim() : '';
        v.quickRollAt = Number(a.quickRollAt) || 0;
        v.deepRollAt = Number(a.deepRollAt) || 0;
        return v;
      }
      case 'relay_speaker_get':
        return state(scope(a)).speaker;
      case 'relay_speaker_save': {
        const sp = state(scope(a)).speaker;
        sp.model = typeof a.model === 'string' ? a.model.trim() : '';
        sp.effort = typeof a.effort === 'string' ? a.effort.trim() : '';
        if (typeof a.takeSpeaker === 'boolean') sp.takeSpeaker = a.takeSpeaker;
        sp.previousHours = Number(a.previousHours) || 0;
        if (typeof a.lean === 'boolean') sp.lean = a.lean;
        if (typeof a.leanPrompt === 'string') sp.leanPrompt = a.leanPrompt.trim();
        // Like the host: the list arrives only from a chip edit, and that saves a custom list.
        if (Array.isArray(a.leanTools)) {
          sp.leanTools = (a.leanTools as unknown[]).filter((t): t is string => typeof t === 'string');
          sp.leanToolsCustom = true;
        }
        if (typeof a.boardId === 'string') sp.boardId = a.boardId.trim();
        sp.quickModel = typeof a.quickModel === 'string' ? a.quickModel.trim() : '';
        sp.quickEffort = typeof a.quickEffort === 'string' ? a.quickEffort.trim() : '';
        sp.quickRollAt = Number(a.quickRollAt) || 0;
        sp.deepRollAt = Number(a.deepRollAt) || 0;
        return sp;
      }
      case 'relay_speaker_reset': {
        const sp = state(scope(a)).speaker;
        const ended = sp.talks.length;
        sp.talks = [];
        return { ...sp, ended };
      }
      case 'relay_prompts_get':
      case 'relay_prompts_save': {
        const saved = state(scope(a)).prompts;
        const kinds = [
          ['speaker', 'Speaker'],
          ['household', 'Household speaker'],
          ['voice', 'Voice app'],
          ['telegram', 'Telegram'],
        ] as const;
        if (op === 'relay_prompts_save') {
          const text = typeof a.text === 'string' ? a.text.trim() : '';
          const channel = String(a.channel);
          if (!kinds.some(([k]) => k === channel)) throw new ApiError('bad_args', 'Expected channel to be speaker, household, voice or telegram.');
          if (text === '' || text === `Default ${channel} prompt.`) delete saved[channel];
          else saved[channel] = text;
        }
        return {
          maxChars: 8000,
          channels: kinds.map(([channel, label]) => ({
            channel,
            label,
            default: `Default ${channel} prompt.`,
            text: saved[channel] ?? `Default ${channel} prompt.`,
            custom: channel in saved,
          })),
        };
      }
      case 'relay_voice_reset': {
        const st = state(scope(a));
        st.voice.chat = null;
        return st.voice;
      }
      case 'relay_tg_link': {
        const st = state(scope(a));
        if (st.linked) throw new ApiError('refused', 'This profile’s person is already linked.');
        // The phone "taps" after four seconds, so the pane's link row shows the linked state.
        setTimeout(() => {
          st.linked = true;
        }, 4000);
        return { url: 'https://t.me/helm_demo_bot?start=DEMO0001', expiresAt: Date.now() + 15 * 60_000 };
      }
      case 'relay_tg_status': {
        const st = state(scope(a));
        return st.linked ? { linked: true, handle: '@helm_demo_bot' } : { linked: false };
      }
      case 'relay_tg_unlink': {
        const st = state(scope(a));
        const had = st.linked;
        st.linked = false;
        return { unlinked: had ? 1 : 0 };
      }
      case 'relay_route_pin': {
        const st = state(scope(a));
        const channel = a.channel === 'voice' ? 'voice' : a.channel === 'telegram' ? 'telegram' : null;
        if (!channel) throw new ApiError('bad_args', 'Expected channel to be telegram or voice.');
        if (!st.pinned.includes(channel)) st.pinned.push(channel);
        return { pinned: true };
      }
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['relay_', 'launcher_', 'hub_pair'], handler };
}
