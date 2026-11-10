// The dev fake host's hub ops (Settings → Hub in a plain browser): a connection that goes
// "connecting" then "connected" once an address and token are saved, and a Telegram chat and a
// voice chat with the default limits. Pairing with the code ABCD2345 works. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { HubConnection, TelegramSettings, VoiceSettings } from '../api/hub.ts';
import type { FakeOpHandler } from './fakeHost.ts';

/** `?household=` in the page URL starts the fake already paired to a hub (see fake-profiles.ts). */
export function pairedAtStart(): boolean {
  return typeof location !== 'undefined' && new URLSearchParams(location.search).has('household');
}

export function hubFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const set = { lanUrl: pairedAtStart(), fallbackUrl: false, token: pairedAtStart() };
  let who: { personName: string; machineName: string } | null = pairedAtStart() ? { personName: 'Alex', machineName: 'Desk PC' } : null;
  let since = 0;
  const tg: TelegramSettings = { chat: null, maxAgeHours: 24, maxTokens: 400_000, awayAlerts: true, approvals: false, model: null, effort: null };
  const voice: VoiceSettings = { chat: null, model: 'claude-sonnet-5-5', effort: 'low', idleMinutes: 240, maxAgeHours: 24, maxTokens: 300_000, watch: 'flags' };
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
      case 'relay_telegram_get':
        if (!tg.chat) {
          const first = (ctx.call('chat_list', {}) as { id: string; title: string }[])[0];
          if (first) tg.chat = { id: first.id, title: first.title, startedAt: Date.now() - 3_600_000, tokensUsed: 12_400 };
        }
        return tg;
      case 'relay_telegram_save':
        tg.maxAgeHours = Number(a.maxAgeHours);
        tg.maxTokens = Number(a.maxTokens);
        tg.awayAlerts = a.awayAlerts !== false;
        tg.approvals = a.approvals === true;
        // Blank or missing counts as unset, as the host reads it.
        tg.model = typeof a.model === 'string' && a.model.trim() ? a.model.trim() : null;
        tg.effort = typeof a.effort === 'string' && a.effort.trim() ? a.effort.trim() : null;
        return tg;
      case 'relay_telegram_reset':
        tg.chat = null;
        return tg;
      case 'relay_voice_get':
        return voice;
      case 'relay_voice_save':
        voice.model = typeof a.model === 'string' ? a.model.trim() : '';
        voice.effort = typeof a.effort === 'string' ? a.effort.trim() : '';
        voice.idleMinutes = Number(a.idleMinutes);
        voice.maxAgeHours = Number(a.maxAgeHours);
        voice.maxTokens = Number(a.maxTokens);
        voice.watch = a.watch === 'off' || a.watch === 'flags+done' ? a.watch : 'flags';
        return voice;
      case 'relay_voice_reset':
        voice.chat = null;
        return voice;
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['relay_', 'hub_pair'], handler };
}
