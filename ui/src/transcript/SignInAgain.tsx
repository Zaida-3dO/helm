// The offer under a failed turn whose cause is an expired Claude sign-in: one button that runs the
// unmodified `claude auth login` in the terminal dock, where the user finishes it themselves.

import { useState } from 'react';
import { terminalOps } from '../api/terminal.ts';
import { useShell } from '../shell/context.ts';
import { adoptSession } from '../terminal/adopt.ts';
import { Button, toast } from '../ui/index.ts';
import './signin.css';

export function SignInAgain() {
  const shell = useShell();
  const [busy, setBusy] = useState(false);
  const start = () => {
    setBusy(true);
    terminalOps(shell.api)
      .claudeSignIn()
      .then(adoptSession)
      .catch((e: unknown) => toast.show({ kind: 'error', text: e instanceof Error ? e.message : String(e) }))
      .finally(() => setBusy(false));
  };
  return (
    <div className="signin-offer" data-testid="error-signin-offer">
      <span className="muted">Claude&apos;s sign-in has expired. Sign in again, then send your message again.</span>
      <Button variant="secondary" busy={busy} onClick={start} data-testid="error-signin">
        Sign in again
      </Button>
    </div>
  );
}
