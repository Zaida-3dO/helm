// The saved Claude token's status (never the token): read once, and kept current by the
// `claude_token` event the host sends when one is saved or forgotten.

import { useEffect, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import type { ClaudeTokenStatus } from './model.ts';

export function useClaudeToken(): ClaudeTokenStatus | null {
  const shell = useShell();
  const [status, setStatus] = useState<ClaudeTokenStatus | null>(null);
  useEffect(() => {
    let live = true;
    void shell
      .api<ClaudeTokenStatus>('claude_token_status', {})
      .then((s) => live && setStatus(s))
      .catch(() => undefined);
    const off = shell.bus.on('claude_token', (e) => {
      const s = e.status as ClaudeTokenStatus | undefined;
      if (s && typeof s.saved === 'boolean') setStatus(s);
    });
    return () => {
      live = false;
      off();
    };
  }, [shell]);
  return status;
}
