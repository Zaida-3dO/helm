// The offer under a failed turn whose cause is an expired Claude sign-in: one button that runs the
// unmodified `claude setup-token` (when chats sign in with a saved token) or `claude auth login` in
// the terminal dock, where the user finishes it themselves.

import { useState } from 'react';
import { terminalOps } from '../api/terminal.ts';
import { useShell } from '../shell/context.ts';
import { useClaudeToken } from '../settings/runtime/useClaudeToken.ts';
import { adoptSession } from '../terminal/adopt.ts';
import { Button, toast } from '../ui/index.ts';
import './signin.css';

/** `message` replaces the default words when the cause is not an expired sign-in (a token refresh
 * that kept failing, say). `login` asks for the normal sign-in even when chats have a saved token
 * (the usage meter reads usage through the normal one). */
export function SignInAgain({ message, login }: { message?: string; login?: boolean }) {
  const shell = useShell();
  const [busy, setBusy] = useState(false);
  const saved = useClaudeToken()?.saved === true;
  const token = saved && !login;
  const start = () => {
    setBusy(true);
    terminalOps(shell.api)
      .claudeSignIn(100, 24, token)
      .then(adoptSession)
      .catch((e: unknown) => toast.show({ kind: 'error', text: e instanceof Error ? e.message : String(e) }))
      .finally(() => setBusy(false));
  };
  return (
    <div className="signin-offer" data-testid="error-signin-offer">
      <span className="muted">
        {message ??
          (token
            ? 'The saved Claude token was refused. Sign in again to make a new one, then send your message again.'
            : "Claude's sign-in has expired. Sign in again, then send your message again.")}
      </span>
      <Button variant="secondary" busy={busy} onClick={start} data-testid="error-signin">
        Sign in again
      </Button>
    </div>
  );
}
