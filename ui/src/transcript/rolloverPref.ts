// "Ask a chat to roll over at … tokens": read live, so the context line's offer matches the
// host's own automatic ask (Settings → Chat). The parsing itself lives with the other context
// facts in rowMeta.ts.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { parseRolloverTokens, ROLL_OVER_TOKENS } from './rowMeta.ts';

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
