// The dev fake host's hub ops (Settings → Hub in a plain browser): a connection that goes
// "connecting" then "connected" once an address and token are saved, and a Telegram chat with
// the default limits. Dev builds only.

import { ApiError } from '../api/client.ts';
import type { HubConnection, TelegramSettings } from '../api/hub.ts';
import type { FakeOpHandler } from './fakeHost.ts';

export function hubFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const set = { lanUrl: false, fallbackUrl: false, token: false };
  let since = 0;
  const tg: TelegramSettings = { chat: null, maxAgeHours: 24, maxTokens: 400_000 };
  const view = (): HubConnection => ({
    hasAddress: set.lanUrl,
    hasFallback: set.fallbackUrl,
    hasToken: set.token,
    state: !(set.lanUrl || set.fallbackUrl) || !set.token ? 'off' : Date.now() - since < 3000 ? 'connecting' : 'online',
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
      case 'relay_telegram_get':
        if (!tg.chat) {
          const first = (ctx.call('chat_list', {}) as { id: string; title: string }[])[0];
          if (first) tg.chat = { id: first.id, title: first.title, startedAt: Date.now() - 3_600_000, tokensUsed: 12_400 };
        }
        return tg;
      case 'relay_telegram_save':
        tg.maxAgeHours = Number(a.maxAgeHours);
        tg.maxTokens = Number(a.maxTokens);
        return tg;
      case 'relay_telegram_reset':
        tg.chat = null;
        return tg;
      default:
        throw new ApiError('unknown_op', op);
    }
  };
  return { prefixes: ['relay_'], handler };
}
