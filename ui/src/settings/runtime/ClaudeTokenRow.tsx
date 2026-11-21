// Settings → Runtime: how chats sign in to Claude. With a saved token, chats use it and never
// refresh the PC's own sign-in; "Sign in" runs `claude setup-token` in the terminal dock, where
// Helm picks the token up when it appears, and a paste box takes one made elsewhere.

import { useState } from 'react';
import { terminalOps } from '../../api/terminal.ts';
import { useShell } from '../../shell/context.ts';
import { adoptSession } from '../../terminal/adopt.ts';
import { Button, SettingRow, TextInput, toast } from '../../ui/index.ts';
import { looksLikeToken, tokenRow } from './model.ts';
import { useClaudeToken } from './useClaudeToken.ts';

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function ClaudeTokenRow() {
  const shell = useShell();
  const status = useClaudeToken();
  const [pasting, setPasting] = useState(false);
  const [text, setText] = useState('');
  const [busy, setBusy] = useState<'sign-in' | 'save' | 'forget' | null>(null);
  const row = tokenRow(status, Date.now());

  const run = (what: 'sign-in' | 'save' | 'forget', job: () => Promise<unknown>, done?: () => void) => {
    setBusy(what);
    job()
      .then(done)
      .catch((e: unknown) => toast.show({ kind: 'error', text: message(e) }))
      .finally(() => setBusy(null));
  };
  const signIn = () => run('sign-in', () => terminalOps(shell.api).claudeSignIn(100, 24, true).then(adoptSession));
  const save = () =>
    run(
      'save',
      () => shell.api('claude_token_save', { token: text.trim() }),
      () => {
        setText('');
        setPasting(false);
      },
    );
  const forget = () => run('forget', () => shell.api('claude_token_forget', {}));
  const saved = status?.saved === true;

  return (
    <SettingRow title={row.title} description={row.note || undefined} testId="claude-token-row">
      <div className="rt-token" data-testid="claude-token" data-state={row.state}>
        <div className="rt-token-buttons">
          <Button variant="secondary" busy={busy === 'sign-in'} onClick={signIn} disabled={!status} data-testid="claude-token-sign-in">
            {saved ? 'Sign in again' : 'Sign in with a token'}
          </Button>
          <Button variant="secondary" onClick={() => setPasting((p) => !p)} disabled={!status} data-testid="claude-token-paste">
            Paste a token
          </Button>
          {saved && (
            <Button variant="secondary" busy={busy === 'forget'} onClick={forget} data-testid="claude-token-forget">
              Forget
            </Button>
          )}
        </div>
        {pasting && (
          <form
            className="rt-token-paste"
            onSubmit={(e) => {
              e.preventDefault();
              if (looksLikeToken(text)) save();
            }}
          >
            <TextInput
              type="password"
              autoComplete="off"
              spellCheck={false}
              placeholder="sk-ant-…"
              value={text}
              onChange={(e) => setText(e.target.value)}
              data-testid="claude-token-input"
              aria-label="Claude token"
            />
            <Button variant="primary" type="submit" busy={busy === 'save'} disabled={!looksLikeToken(text)} data-testid="claude-token-save">
              Save
            </Button>
          </form>
        )}
      </div>
    </SettingRow>
  );
}
