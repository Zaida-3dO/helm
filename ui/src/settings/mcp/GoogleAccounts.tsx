// Settings → MCP, the Google accounts dialog: the one-time sign-in client (GoogleClientSetup),
// then the signed-in addresses, adding and removing one, a label for each, and the permission
// level. The sign-in happens in the browser; Helm keeps only the result, in its credential
// store, and never sees a Google password.

import { useCallback, useEffect, useState } from 'react';
import type { McpEditorSnapshot, McpServer } from '../../api/mcp.ts';
import { Button, ConfirmDialog, Dialog, Segmented, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import { IconWarning } from '../../ui/icons.tsx';
import { WheelMark } from '../../ui/status/index.ts';
import { GoogleClientSetup } from './GoogleClientSetup.tsx';
import {
  PERMISSION_HELP,
  PERMISSION_OPTIONS,
  POLL_MS,
  RAISE_NOTE,
  REMOVE_NOTE,
  SIGN_IN_NOTE,
  blockState,
  watchState,
  type GoogleAccount,
  type GoogleAccountList,
  type GoogleOps,
  type GooglePermission,
  type SignInWatch,
} from './google.ts';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

/** Sign-in asked for the scopes of the level picked then; a raise only takes effect once each
 * account signs in again, so the change says so when it is a raise. */
const LEVEL_RANK: Record<GooglePermission, number> = { readonly: 0, drafts: 1, send: 2 };

export function GoogleAccountsDialog({
  server,
  ops,
  onSnapshot,
  onOpenLink,
  onClose,
}: {
  server: McpServer;
  ops: GoogleOps;
  onSnapshot: (s: McpEditorSnapshot) => void;
  /** Opens a console page in the system browser. */
  onOpenLink: (url: string) => void;
  onClose: () => void;
}) {
  const [list, setList] = useState<GoogleAccountList | null>(null);
  const [configured, setConfigured] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [watch, setWatch] = useState<SignInWatch | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  // The list and the client state come together; either may change under us, so both refresh on
  // the mcp_config event and on the poll below.
  const refresh = useCallback(() => {
    Promise.all([ops.accounts(server.id), ops.clientGet().then((c) => c.configured)])
      .then(([l, c]) => {
        setList(l);
        setConfigured(c);
      })
      .catch((e: unknown) => setError(errorText(e)));
  }, [ops, server.id]);
  useEffect(() => {
    refresh();
  }, [refresh, server]);

  // While a sign-in is open, read the list every 2 s until a new address shows up, the host's
  // listener lets go (a Cancel or Deny at Google), or the 5-minute window ends.
  useEffect(() => {
    if (!watch) return;
    const timer = window.setInterval(() => {
      ops
        .accounts(server.id)
        .then((l) => {
          setList(l);
          const state = watchState(watch, Date.now(), l);
          if (state === 'waiting') return;
          setWatch(null);
          setAuthUrl(null);
          if (state === 'added') setNotice('Account added.');
          else if (state === 'refused') setError('That sign-in was not completed. Try Add account again.');
          else setError('The sign-in did not finish in 5 minutes. Try Add account again.');
        })
        .catch(() => {});
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [watch, ops, server.id]);

  const state = blockState(list, configured);
  // The host keeps one sign-in open at a time: its list says so even if this dialog did not start it.
  const waiting = watch !== null || (list?.signingIn ?? false);

  const attempt = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await f();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      setAdding(false);
    }
  };

  const saveClient = async (clientId: string, secret: string) => {
    let saved = false;
    await attempt(async () => {
      const c = await ops.clientSet(clientId, secret);
      setConfigured(c.configured);
      setNotice('Sign-in client saved.');
      saved = c.configured;
    });
    return saved;
  };

  // One button, no address to type: Google shows its own account chooser.
  const add = () =>
    void attempt(async () => {
      setAdding(true);
      const started = await ops.add(server.id);
      setAuthUrl(started.authUrl ?? null);
      setWatch({
        startedAt: Date.now(),
        known: (list?.accounts ?? []).map((a) => a.email.toLowerCase()),
        sawSigningIn: true,
      });
    });

  const relabel = (a: GoogleAccount, value: string) => {
    if ((a.label ?? '') === value.trim()) return;
    void attempt(async () => setList(await ops.label(server.id, a.email, value)));
  };

  const askRemove = (address: string) =>
    setConfirm({
      title: `Remove ${address}?`,
      body: REMOVE_NOTE,
      confirm: 'Remove',
      danger: true,
      onConfirm: () => attempt(async () => setList(await ops.remove(server.id, address))),
    });

  const setLevel = (level: GooglePermission) =>
    void attempt(async () => {
      const raised = list?.permission != null && LEVEL_RANK[level] > LEVEL_RANK[list.permission];
      onSnapshot(await ops.permission(server.id, level));
      refresh();
      if (raised) setNotice(RAISE_NOTE);
    });

  return (
    <Dialog title={`Google accounts · ${server.name}`} testId="google-accounts-dialog" onClose={onClose}>
      <section className="google-accounts" data-testid="google-accounts" data-server={server.id} data-state={state.kind} aria-label={`${server.name}: accounts`}>
        {state.kind === 'no-uv' && (
          <p className="mcp-error" role="alert" data-testid="google-uv-missing">
            {state.message}
          </p>
        )}
        <GoogleClientSetup configured={configured} busy={busy} onSave={saveClient} onOpenLink={onOpenLink} />
        {list && list.accounts.length > 0 && (
          <ul className="mcp-list" data-testid="google-account-list">
            {list.accounts.map((a) => (
              <li key={`${a.email}:${a.label ?? ''}`} className="mcp-server google-account" data-testid={`google-account-${a.email}`}>
                <div className="mcp-server-main">
                  <span className="mcp-server-name">{a.email}</span>
                </div>
                <TextInput
                  className="google-label"
                  aria-label={`Label for ${a.email}`}
                  data-testid={`google-label-${a.email}`}
                  placeholder="Label (optional)"
                  maxLength={32}
                  defaultValue={a.label ?? ''}
                  onBlur={(e) => relabel(a, e.target.value)}
                />
                <Button className="mcp-act" data-testid={`google-remove-${a.email}`} disabled={busy} onClick={() => askRemove(a.email)}>
                  Remove
                </Button>
              </li>
            ))}
          </ul>
        )}
        {configured === true && (
          <>
            <div className="mcp-actions">
              <Button variant="primary" data-testid="google-add" disabled={busy || adding || waiting} onClick={add}>
                Add account
              </Button>
            </div>
            {adding && (
              <p className="mcp-test" role="status" data-testid="google-adding" data-state="running">
                <WheelMark size={14} state="working" />
                Opening your browser…
              </p>
            )}
            {waiting && !adding && (
              <p className="mcp-test" role="status" data-testid="google-signin-note" data-state="waiting">
                <WheelMark size={14} state="working" />
                Your browser is open: finish signing in there. {SIGN_IN_NOTE}
              </p>
            )}
            {authUrl && (
              <label className="mcp-field google-link">
                <span>If the browser did not open, copy this link into it</span>
                <TextInput data-testid="google-auth-url" readOnly spellCheck={false} value={authUrl} onFocus={(e) => e.target.select()} />
              </label>
            )}
            {list && (
              <>
                <div className="mcp-field google-permission">
                  <span>Chats may</span>
                  <Segmented<GooglePermission | 'custom'>
                    label="What chats may do with mail"
                    testId="google-permission"
                    value={list.permission ?? 'custom'}
                    options={[...PERMISSION_OPTIONS, ...(list.permission ? [] : [{ value: 'custom' as const, label: 'Custom', disabled: true }])]}
                    onChange={(v) => v !== 'custom' && !busy && setLevel(v)}
                  />
                </div>
                <p className="mcp-help" data-testid="google-permission-help">
                  {list.permission
                    ? PERMISSION_HELP[list.permission]
                    : 'The server’s arguments set a level this picker does not offer. Choose one to replace it.'}
                  {!list.hostServed && <> Changing it restarts the server.</>}{' '}
                  {RAISE_NOTE}
                </p>
              </>
            )}
          </>
        )}
        {notice && (
          <p className="mcp-test" role="status" data-testid="google-notice">
            {notice}
          </p>
        )}
        {error && (
          <p className="mcp-error" role="alert" data-testid="google-error">
            <IconWarning size={14} /> {error}
          </p>
        )}
        {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
      </section>
    </Dialog>
  );
}
