// "Ask a chat to roll over at … tokens": read live, so the context line's offer matches the
// host's own automatic ask (Settings → Chat).

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { ROLL_OVER_TOKENS } from './rowMeta.ts';

/** The stored value as the threshold the chat offers a roll over at: a whole number in range;
 * 0 (the automatic ask switched off) and anything unreadable keep the default constant. */
export function parseRolloverTokens(v: unknown): number {
  return typeof v === 'number' && Number.isInteger(v) && v > 0 && v <= 2_000_000
    ? v
    : ROLL_OVER_TOKENS;
}

/** `rolloverTokens`, live: re-read when the preference changes. */
export function useRolloverTokens(): number {
  const shell = useShell();
  const [tokens, setTokens] = useState(ROLL_OVER_TOKENS);
  useEffect(() => {
    void shell
      .api<unknown>('preference_get', { key: 'rolloverTokens' })
      .then((v) => setTokens(parseRolloverTokens(v)))
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const c = (e as { changed?: Record<string, unknown> }).changed ?? {};
      if ('rolloverTokens' in c) setTokens(parseRolloverTokens(c.rolloverTokens));
    });
  }, [shell]);
  return tokens;
}
