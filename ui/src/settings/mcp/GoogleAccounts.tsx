// Settings → MCP, under a Google accounts server: the signed-in addresses, adding and removing
// one, a label for each, and the permission level. Helm never holds a Google password or token:
// the sign-in happens in the browser and the server keeps the result in the profile's folder.

import { useCallback, useEffect, useState } from 'react';
import type { McpEditorSnapshot, McpServer } from '../../api/mcp.ts';
import { Button, ConfirmDialog, Segmented, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import { IconWarning } from '../../ui/icons.tsx';
import { WheelMark } from '../../ui/status/index.ts';
import {
  PERMISSION_HELP,
  PERMISSION_OPTIONS,
  POLL_MS,
  REMOVE_NOTE,
  SETUP_STEPS,
  SIGN_IN_NOTE,
  blockState,
  emailLooksValid,
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

/** The one-time Google Cloud steps. Open by default while the server has no client yet. */
export function SetUpOnce({ open }: { open?: boolean }) {
  return (
    <details className="google-setup" data-testid="google-setup" open={open}>
      <summary>Set up once</summary>
      <ol>
        {SETUP_STEPS.map((s) => (
          <li key={s}>{s}</li>
        ))}
      </ol>
    </details>
  );
}

export function GoogleAccounts({ server, ops, onSnapshot }: { server: McpServer; ops: GoogleOps; onSnapshot: (s: McpEditorSnapshot) => void }) {
  const [list, setList] = useState<GoogleAccountList | null>(null);
  const [email, setEmail] = useState('');
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [watch, setWatch] = useState<SignInWatch | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  const refresh = useCallback(() => {
    ops
      .accounts(server.id)
      .then(setList)
      .catch((e: unknown) => setError(errorText(e)));
  }, [ops, server.id]);
  useEffect(() => {
    refresh();
  }, [refresh, server]);

  // While a sign-in is open, read the list every 2 s until the address shows up or 3 minutes pass.
  useEffect(() => {
    if (!watch) return;
    const timer = window.setInterval(() => {
      ops
        .accounts(server.id)
        .then((l) => {
          setList(l);
          const state = watchState(watch, Date.now(), l.accounts);
          if (state === 'waiting') return;
          setWatch(null);
          setAuthUrl(null);
          if (state === 'added') setNotice(`Added ${watch.email}.`);
          else setError('The sign-in did not finish in 3 minutes. Choose Add account to try again.');
        })
        .catch(() => {});
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [watch, ops, server.id]);

  const state = blockState(server, list);
  const canAdd = !busy && !watch && emailLooksValid(email) && (state.kind === 'empty' || state.kind === 'ready');

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

  const add = () =>
    void attempt(async () => {
      setAdding(true);
      const address = email.trim();
      const started = await ops.add(server.id, address);
      setAuthUrl(started.authUrl ?? null);
      setWatch({ startedAt: Date.now(), email: address });
      setEmail('');
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
      onSnapshot(await ops.permission(server.id, level));
      refresh();
    });

  return (
    <section className="google-accounts" data-testid="google-accounts" data-server={server.id} data-state={state.kind} aria-label={`${server.name}: accounts`}>
      <h4>{server.name}: accounts</h4>
      {state.kind === 'no-uv' && (
        <p className="mcp-error" role="alert" data-testid="google-uv-missing">
          {state.message}
        </p>
      )}
      {state.kind === 'no-client' && (
        <p className="mcp-help" data-testid="google-client-missing">
          {state.message}
        </p>
      )}
      <SetUpOnce open={state.kind === 'no-client'} />
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
      {state.kind === 'empty' && (
        <p className="mcp-help" data-testid="google-empty">
          No accounts yet.
        </p>
      )}
      {(state.kind === 'empty' || state.kind === 'ready') && (
        <>
          <div className="google-add">
            <TextInput
              data-testid="google-email"
              type="email"
              aria-label="Gmail address to add"
              placeholder="name@gmail.com"
              spellCheck={false}
              autoComplete="off"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canAdd) add();
              }}
            />
            <Button variant="primary" data-testid="google-add" disabled={!canAdd} onClick={add}>
              Add account
            </Button>
          </div>
          {adding && (
            <p className="mcp-test" role="status" data-testid="google-starting">
              <WheelMark size={14} state="working" />
              Starting the server. The first time can take a minute while uv downloads it.
            </p>
          )}
          {watch ? (
            <p className="mcp-test" role="status" data-testid="google-signin-note" data-state="waiting">
              <WheelMark size={14} state="working" />
              Waiting for you to finish signing in for {watch.email}. {SIGN_IN_NOTE}
            </p>
          ) : (
            <p className="mcp-help">{SIGN_IN_NOTE}</p>
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
                  : 'The server’s arguments set a level this picker does not offer. Choose one to replace it.'}{' '}
                Changing it restarts the server.
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
  );
}
