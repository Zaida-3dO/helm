// Settings → MCP. The server list with each
// server's live status and readiness, an editor, and presets. Secrets are write-only: the pane
// only ever sees a saved secret's id, never its value.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { mcpOps, type McpPreset, type McpServer, type RefKind, type RepoServer, type ServerStatus, type TestOutcome, type ValueRef } from '../../api/mcp.ts';
import { shellOps } from '../../api/shell.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Segmented, TextArea, TextInput, toast } from '../../ui/index.ts';
import { IconCheck, IconWarning } from '../../ui/icons.tsx';
import { WheelMark } from '../../ui/status/index.ts';
import { GoogleAccountsDialog } from './GoogleAccounts.tsx';
import { GOOGLE_PRESET, googleOps, isGoogleServer } from './google.ts';
import { WAITING_NOTE, canDisconnect, connectLabel, isOAuthServer, oauthOps, type OAuthStatus } from './oauth.ts';
import { blankServer, draftFromPreset, isPlainRemoteHttp, pageReadiness, serverReadiness, toSaveable } from './readiness.ts';

type Refs = { [key in string]?: ValueRef };

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

function RefRows({ refs, onChange, kindLabel, headers = false }: { refs: Refs; onChange: (r: Refs) => void; kindLabel: string; headers?: boolean }) {
  const kinds: { value: RefKind; label: string }[] = [
    { value: 'literal', label: 'Secret' },
    { value: 'environment', label: 'Env var' },
    { value: 'file', label: 'File' },
  ];
  // Only a header can carry the chat's person: Helm fills it per chat when it connects.
  if (headers) kinds.push({ value: 'chatPerson', label: "Chat's person" });
  const rows = Object.entries(refs);
  const set = (oldKey: string, key: string, r: ValueRef) => {
    const next: Refs = {};
    for (const [k, v] of rows) next[k === oldKey ? key : k] = k === oldKey ? r : v;
    onChange(next);
  };
  const drop = (key: string) => {
    const next: Refs = { ...refs };
    delete next[key];
    onChange(next);
  };
  return (
    <div className="mcp-refs">
      {rows.map(([k, r], i) =>
        r ? (
          <div className="mcp-ref" key={i} data-testid={`mcp-ref-${k}`}>
            <TextInput aria-label={`${kindLabel} name`} value={k} spellCheck={false} onChange={(e) => set(k, e.target.value, r)} />
            <Segmented<RefKind>
              label="Value kind"
              value={r.kind}
              options={kinds}
              onChange={(kind) =>
                set(k, k, kind === 'chatPerson' ? { kind } : { kind, value: '', ...(r.prefix ? { prefix: r.prefix } : {}) })
              }
            />
            {r.kind === 'chatPerson' ? (
              <span className="mcp-help">
                Sent as the chat's person
              </span>
            ) : (
              <TextInput
                aria-label={`${kindLabel} value`}
                type={r.kind === 'literal' ? 'password' : 'text'}
                autoComplete="off"
                spellCheck={false}
                value={r.value ?? ''}
                placeholder={
                  r.kind === 'literal' ? (r.secretId ? 'Saved; type to replace' : 'Not set') : r.kind === 'environment' ? 'VARIABLE_NAME' : 'C:\\path\\to\\token.txt'
                }
                onChange={(e) => set(k, k, { ...r, value: e.target.value, ...(r.kind === 'literal' && e.target.value ? { secretId: undefined } : {}) })}
              />
            )}
            <Button variant="text" danger onClick={() => drop(k)}>
              Remove
            </Button>
          </div>
        ) : null,
      )}
      <Button variant="text" onClick={() => onChange({ ...refs, '': { kind: 'literal', value: '' } })}>
        Add {kindLabel.toLowerCase()}
      </Button>
    </div>
  );
}

function Editor({
  draft,
  isNew,
  busy,
  onChange,
  onSave,
  onCancel,
  onTest,
  preset,
  signIn,
}: {
  draft: McpServer;
  isNew: boolean;
  busy: boolean;
  onChange: (d: McpServer) => void;
  onSave: () => void;
  onCancel: () => void;
  /** Connect once with the draft as it stands and report what happened. */
  onTest: (server: McpServer) => Promise<TestOutcome>;
  preset?: McpPreset;
  /** For a saved server that signs in through the browser: the button beside Test connection. */
  signIn?: { label: string; waiting: boolean; connect: () => void };
}) {
  const t = draft.transport;
  const setT = (patch: Partial<McpServer['transport']>) => onChange({ ...draft, transport: { ...t, ...patch } });
  const remote = t.type !== 'stdio';
  // Opening the editor must be visible: scroll it into view and put the caret in its first field.
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.scrollIntoView({ block: 'start' });
    el.querySelector<HTMLElement>('input, textarea')?.focus();
  }, []);
  // The last connection test. Any edit to the draft makes its answer stale, so it is dropped.
  const [test, setTest] = useState<{ running: boolean; outcome?: TestOutcome; failed?: string }>({ running: false });
  const run = useRef(0);
  useEffect(() => {
    run.current++;
    setTest({ running: false });
  }, [draft]);
  const runTest = () => {
    const mine = ++run.current;
    setTest({ running: true });
    onTest(toSaveable(draft))
      .then((outcome) => mine === run.current && setTest({ running: false, outcome }))
      .catch((e: unknown) => mine === run.current && setTest({ running: false, failed: errorText(e) }));
  };
  return (
    <div className="mcp-editor" data-testid="mcp-editor">
      <h4>{isNew ? 'Add a server' : `Edit ${draft.name}`}</h4>
      {preset && <p className="mcp-help">{preset.description}</p>}
      {preset?.secretEnv?.length ? (
        preset.id === GOOGLE_PRESET ? (
          <p className="mcp-help">
            Your Google sign-in client and accounts are set in the Google accounts dialog, not here. Leave Environment empty unless this server is an older
            one that carries its own client.
          </p>
        ) : (
          <p className="mcp-help">Under Environment, enter {preset.secretEnv.join(' and ')} as secrets.</p>
        )
      ) : null}
      <label className="mcp-field">
        <span>Name</span>
        <TextInput data-testid="mcp-name" value={draft.name} onChange={(e) => onChange({ ...draft, name: e.target.value })} />
      </label>
      {isNew && (
        <label className="mcp-field">
          <span>Id</span>
          <TextInput data-testid="mcp-id" value={draft.id} spellCheck={false} onChange={(e) => onChange({ ...draft, id: e.target.value })} />
        </label>
      )}
      <div className="mcp-field">
        <span>Connects by</span>
        <Segmented<'stdio' | 'http'>
          label="Transport"
          testId="mcp-transport"
          value={t.type === 'stdio' ? 'stdio' : 'http'}
          options={[
            { value: 'stdio', label: 'Program on this PC' },
            { value: 'http', label: 'URL' },
          ]}
          onChange={(v) => setT({ type: v })}
        />
      </div>
      {remote ? (
        <>
          <label className="mcp-field">
            <span>URL</span>
            <TextInput
              data-testid="mcp-url"
              type="url"
              spellCheck={false}
              value={t.url ?? ''}
              placeholder={preset?.urlPlaceholder ?? 'https://server.example/mcp'}
              onChange={(e) => setT({ url: e.target.value })}
            />
          </label>
          <div className="mcp-field mcp-field-top">
            <span>Headers</span>
            <RefRows refs={t.headers ?? {}} kindLabel="Header" headers onChange={(headers) => setT({ headers })} />
          </div>
          <div className="mcp-field">
            <span />
            <Checkbox
              data-testid="mcp-allow-private"
              checked={draft.allowPrivateHttp}
              onChange={(allowPrivateHttp) => onChange({ ...draft, allowPrivateHttp })}
              label="Allow unencrypted on my network"
            />
          </div>
          {(draft.allowPrivateHttp || preset?.suggestPrivateHttp) && (
            <p className="mcp-help">
              Plain http is allowed only to this computer or a private-network address (home network or Tailscale). With it on, the token
              travels unencrypted on your local network.
            </p>
          )}
          {t.oauth && (
            <div className="mcp-oauth" data-testid="mcp-oauth">
              <p className="mcp-help" data-testid="mcp-oauth-note">
                This server signs in through your browser.{' '}
                {isNew ? 'Save it first; Connect then opens the sign-in.' : 'Connect opens the sign-in.'}
              </p>
              <label className="mcp-field">
                <span>Client id</span>
                <TextInput
                  data-testid="mcp-oauth-client-id"
                  spellCheck={false}
                  autoComplete="off"
                  placeholder="Only if the server cannot register automatically"
                  value={t.oauth.clientId ?? ''}
                  onChange={(e) => setT({ oauth: { ...t.oauth, clientId: e.target.value } })}
                />
              </label>
              <label className="mcp-field">
                <span>Client secret</span>
                <TextInput
                  data-testid="mcp-oauth-client-secret"
                  type="password"
                  spellCheck={false}
                  autoComplete="off"
                  placeholder={t.oauth.clientSecret?.kind === 'literal' && t.oauth.clientSecret.secretId ? 'Saved; type to replace' : 'Usually left empty'}
                  value={t.oauth.clientSecret?.kind === 'literal' ? t.oauth.clientSecret.value ?? '' : ''}
                  onChange={(e) => setT({ oauth: { ...t.oauth, clientSecret: { kind: 'literal', value: e.target.value } } })}
                />
              </label>
            </div>
          )}
        </>
      ) : (
        <>
          <label className="mcp-field">
            <span>Program</span>
            <TextInput
              data-testid="mcp-command"
              spellCheck={false}
              value={t.command ?? ''}
              placeholder="node.exe, uvx.exe, npx …"
              onChange={(e) => setT({ command: e.target.value })}
            />
          </label>
          <label className="mcp-field mcp-field-top">
            <span>Arguments</span>
            <TextArea
              data-testid="mcp-args"
              rows={3}
              spellCheck={false}
              value={(t.args ?? []).join('\n')}
              placeholder="One per line"
              onChange={(e) => setT({ args: e.target.value === '' ? [] : e.target.value.split('\n') })}
            />
          </label>
          <div className="mcp-field mcp-field-top">
            <span>Environment</span>
            <RefRows refs={t.env ?? {}} kindLabel="Variable" onChange={(env) => setT({ env })} />
          </div>
        </>
      )}
      <p className="mcp-help">Saving does not connect. A chat connects the first time it uses the server.</p>
      <div className="mcp-actions">
        <Button variant="primary" data-testid="mcp-save" disabled={busy} onClick={onSave}>
          Save
        </Button>
        <Button data-testid="mcp-test" disabled={test.running} onClick={runTest}>
          {test.running ? 'Testing…' : 'Test connection'}
        </Button>
        {signIn && (
          <Button data-testid="mcp-oauth-connect" disabled={signIn.waiting} onClick={signIn.connect}>
            {signIn.waiting ? 'Waiting for your browser…' : signIn.label}
          </Button>
        )}
        <Button onClick={onCancel}>Cancel</Button>
      </div>
      {test.running && (
        <p className="mcp-test" data-testid="mcp-test-result" data-state="running" role="status">
          <WheelMark size={14} state="working" />
          Connecting…
        </p>
      )}
      {(test.outcome || test.failed) && (
        <p className="mcp-test" data-testid="mcp-test-result" data-state={test.outcome?.ok ? 'ok' : 'failed'} role="status">
          {test.outcome?.ok ? <IconCheck size={14} /> : <IconWarning size={14} />}
          {test.outcome?.message ?? `The test could not run: ${test.failed}`}
        </p>
      )}
      {/* A refused test that wants a sign-in offers the browser route instead of a token field. */}
      {test.outcome && !test.outcome.ok && test.outcome.signinRequired && remote && !t.oauth && (
        <div className="mcp-oauth-offer" data-testid="mcp-oauth-offer">
          <p className="mcp-help">This server signs in through your browser — no token to paste.</p>
          <Button variant="primary" data-testid="mcp-oauth-use" onClick={() => setT({ oauth: {} })}>
            Use browser sign-in
          </Button>
        </div>
      )}
    </div>
  );
}

export function McpPane() {
  const shell = useShell();
  const ops = useMemo(() => mcpOps(shell.api), [shell.api]);
  const google = useMemo(() => googleOps(shell.api), [shell.api]);
  const oauth = useMemo(() => oauthOps(shell.api), [shell.api]);
  const [servers, setServers] = useState<McpServer[]>([]);
  // Per browser-signed-in server: whether a sign-in is kept, and an open or refused sign-in ('' = waiting).
  const [oauthState, setOauthState] = useState<Record<string, OAuthStatus>>({});
  const [signInNote, setSignInNote] = useState<Record<string, string>>({});
  const [revision, setRevision] = useState<string | undefined>();
  const [presets, setPresets] = useState<McpPreset[]>([]);
  const [status, setStatus] = useState<Record<string, ServerStatus>>({});
  const [draft, setDraft] = useState<{ server: McpServer; isNew: boolean } | null>(null);
  // The Google accounts dialog: the id of the server it is open for, or null.
  const [googleDialog, setGoogleDialog] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [native, setNative] = useState(true);
  const [allOn, setAllOn] = useState(true);
  const [repo, setRepo] = useState<RepoServer[]>([]);

  const reload = useCallback(() => {
    ops
      .get()
      .then((s) => {
        setServers(s.servers);
        setRevision(s.revision);
      })
      .catch((e: unknown) => setError(errorText(e)));
  }, [ops]);
  const reloadStatus = useCallback(() => {
    ops
      .status()
      .then((list) => setStatus(Object.fromEntries(list.map((s) => [s.serverId, s]))))
      .catch(() => {});
  }, [ops]);

  // Whether each browser-signed-in server has a sign-in kept (never any token material).
  useEffect(() => {
    const ids = servers.filter(isOAuthServer).map((s) => s.id);
    if (ids.length === 0) return;
    void Promise.all(ids.map((id) => oauth.status(id).then((st) => ({ id, st })).catch(() => null))).then(
      (seen) =>
        setOauthState((prev) => {
          const next = { ...prev };
          for (const s of seen) if (s) next[s.id] = s.st;
          return next;
        }),
    );
  }, [servers, oauth]);

  /** Open the browser sign-in for one saved server; it answers when the browser does, or refuses. */
  const startSignIn = (id: string) => {
    setSignInNote((w) => ({ ...w, [id]: '' }));
    oauth
      .connect(id)
      .then((st) => {
        setOauthState((o) => ({ ...o, [id]: st }));
        setSignInNote((w) => {
          const n = { ...w };
          delete n[id];
          return n;
        });
        reloadStatus();
      })
      .catch((e: unknown) => setSignInNote((w) => ({ ...w, [id]: errorText(e) })));
  };

  /** Forget the kept sign-in for one saved server (the next use asks to sign in again). */
  const disconnect = (id: string) => {
    oauth
      .forget(id)
      .then(() => {
        setOauthState((o) => ({ ...o, [id]: { connected: false } }));
        setSignInNote((w) => {
          const n = { ...w };
          delete n[id];
          return n;
        });
        reloadStatus();
      })
      .catch((e: unknown) => setSignInNote((w) => ({ ...w, [id]: errorText(e) })));
  };

  useEffect(() => {
    reload();
    reloadStatus();
    ops.nativeOn().then(setNative).catch(() => {});
    ops.allOn().then(setAllOn).catch(() => {});
    ops.repoList().then(setRepo).catch(() => {});
    ops
      .presets()
      .then(setPresets)
      .catch(() => {});
    const offs = [shell.bus.on('mcp_config', () => reload()), shell.bus.on('mcp_status', () => reloadStatus())];
    return () => offs.forEach((o) => o());
  }, [ops, shell.bus, reload, reloadStatus]);

  const run = async (f: () => Promise<{ servers: McpServer[]; revision: string }>) => {
    setBusy(true);
    setError(null);
    try {
      const s = await f();
      setServers(s.servers);
      setRevision(s.revision);
      return true;
    } catch (e) {
      setError(errorText(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    if (!draft) return;
    if (await run(() => ops.save(toSaveable(draft.server), revision))) setDraft(null);
  };

  const ids = servers.map((s) => s.id);
  const openLink = (url: string) => void shellOps(shell.api).openExternal(url).catch((e: unknown) => toast.show({ kind: 'error', text: errorText(e) }));

  /** Clicking a preset must open its setup visibly: a Google server goes straight to its dialog (its draft has no choices to make), the rest to the editor. */
  const openPreset = (p: McpPreset) => {
    const server = draftFromPreset(p, ids);
    if (p.id === GOOGLE_PRESET) {
      void run(() => ops.save(toSaveable(server), revision)).then((ok) => ok && setGoogleDialog(server.id));
      return;
    }
    setDraft({ server, isNew: true });
  };

  const empty = pageReadiness(servers);
  return (
    <div className="mcp-pane" data-testid="mcp-pane">
      <section aria-labelledby="mcp-master" data-testid="mcp-master">
        <h3 id="mcp-master">MCP in chats</h3>
        <Checkbox
          data-testid="mcp-master-switch"
          checked={allOn}
          onChange={(on) => {
            setAllOn(on);
            ops.setAll(on).catch((e: unknown) => setError(errorText(e)));
          }}
          label="Let chats use MCP servers"
        />
        <p className="mcp-help" data-testid="mcp-harnesses">
          {allOn
            ? 'Chats on Claude, and chats on a local model (which also run through Claude Code), connect to the servers listed below through Helm. Claude Code may add its own servers too; the next section controls that. Takes effect for chats started from now on.'
            : 'No chat loads any MCP server: not the ones listed below and not Claude Code’s own. Takes effect for chats started from now on.'}
        </p>
      </section>
      <section aria-labelledby="mcp-servers">
        <h3 id="mcp-servers">MCP servers</h3>
        <p className="mcp-help">
          Servers Helm connects chats to through its own client. When "Use my Claude Code MCP servers" is on and Claude Code
          already loads a server of the same name, chats use Claude Code's copy and skip the one listed here. Servers here are shared with the chats of every project, and a server you add can read whatever you give it access
          to, so add only ones you trust.
        </p>
        {empty && (
          <div className="mcp-empty" data-testid="mcp-empty" data-state={empty.state}>
            {empty.reason}
          </div>
        )}
        <ul className="mcp-list">
          {servers.map((s) => {
            const r = serverReadiness(s, status[s.id], presets, oauthState[s.id]);
            const signInLabel = isOAuthServer(s) ? connectLabel(oauthState[s.id], status[s.id]?.state) : undefined;
            // An empty note is a sign-in that is open; a worded one is what went wrong.
            const waiting = signInNote[s.id] === '';
            return (
              <li key={s.id} className="mcp-server" data-testid={`mcp-server-${s.id}`} data-state={r.state}>
                <div className="mcp-server-main">
                  <span className="mcp-server-name">{s.name}</span>
                  <span className="mcp-meta">
                    {s.transport.type === 'stdio' ? s.transport.command : s.transport.url}
                    {s.allowPrivateHttp && isPlainRemoteHttp(s.transport.url) ? ' · unencrypted on your network' : ''}
                  </span>
                  <span className={`mcp-status mcp-status-${r.state}`} data-testid={`mcp-status-${s.id}`}>
                    {r.reason}
                  </span>
                </div>
                <Checkbox
                  aria-label={`Use ${s.name}`}
                  checked={s.enabled}
                  disabled={busy}
                  onChange={(on) => void run(() => ops.setEnabled(s.id, on, revision))}
                  label="On"
                />
                {signInLabel && (
                  <Button className="mcp-act" data-testid={`mcp-oauth-${s.id}`} disabled={busy || waiting} onClick={() => startSignIn(s.id)}>
                    {waiting ? 'Waiting…' : signInLabel}
                  </Button>
                )}
                {isOAuthServer(s) && canDisconnect(oauthState[s.id]) && (
                  <Button className="mcp-act" data-testid={`mcp-oauth-disconnect-${s.id}`} disabled={busy || waiting} onClick={() => disconnect(s.id)}>
                    Disconnect
                  </Button>
                )}
                {isGoogleServer(s) && (
                  <Button className="mcp-act" data-testid={`mcp-google-${s.id}`} onClick={() => setGoogleDialog(s.id)}>
                    Accounts
                  </Button>
                )}
                <Button className="mcp-act" onClick={() => setDraft({ server: structuredClone(s), isNew: false })}>
                  Edit
                </Button>
                <Button className="mcp-act" disabled={busy} onClick={() => void run(() => ops.remove(s.id, revision))}>
                  Remove
                </Button>
                {signInNote[s.id] !== undefined && (
                  <p className="mcp-test" data-testid={`mcp-oauth-note-${s.id}`} data-state={signInNote[s.id] ? 'failed' : 'running'} role="status">
                    {signInNote[s.id] ? signInNote[s.id] : (<><WheelMark size={14} state="working" /> {WAITING_NOTE}</>)}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
        {googleDialog && servers.some((s) => s.id === googleDialog && isGoogleServer(s)) && (
          <GoogleAccountsDialog
            server={servers.find((s) => s.id === googleDialog)!}
            ops={google}
            onOpenLink={openLink}
            onClose={() => setGoogleDialog(null)}
            onSnapshot={(snap) => {
              setServers(snap.servers);
              setRevision(snap.revision);
            }}
          />
        )}
        {error && (
          <p className="mcp-error" role="alert">
            {error}
          </p>
        )}
        {draft && (
          <Editor
            draft={draft.server}
            isNew={draft.isNew}
            busy={busy}
            preset={presets.find((p) => p.id === draft.server.preset)}
            onChange={(server) => setDraft({ ...draft, server })}
            onSave={() => void save()}
            onCancel={() => setDraft(null)}
            onTest={ops.test}
            signIn={
              !draft.isNew && isOAuthServer(draft.server)
                ? {
                    label: connectLabel(oauthState[draft.server.id], status[draft.server.id]?.state) ?? 'Sign in again',
                    waiting: signInNote[draft.server.id] === '',
                    connect: () => startSignIn(draft.server.id),
                  }
                : undefined
            }
          />
        )}
      </section>
      <section aria-labelledby="mcp-native" data-testid="mcp-native">
        <h3 id="mcp-native">Claude Code's own servers</h3>
        <Checkbox
          data-testid="mcp-native-switch"
          checked={native}
          onChange={(on) => {
            setNative(on);
            ops.setNative(on).catch((e: unknown) => setError(errorText(e)));
          }}
          label="Use my Claude Code MCP servers"
        />
        <p className="mcp-help">
          {native
            ? 'Chats also load the servers Claude Code loads for you: your own, the claude.ai connectors and approved repository servers. Helm skips its own copy of any server Claude Code already loads,.'
            : 'Chats load only the servers listed above. Takes effect for chats started from now on.'}
        </p>
        {native && (
          <>
            <h4>Repository servers</h4>
            {repo.length === 0 ? (
              <p className="mcp-help" data-testid="mcp-repo-empty">
                None seen yet. A repository's servers are listed here once a chat starts in its folder.
              </p>
            ) : (
              <ul className="mcp-list">
                {repo.map((r) => (
                  <li key={`${r.file}|${r.name}`} className="mcp-server" data-testid={`mcp-repo-${r.name}`} data-state={r.state}>
                    <div className="mcp-server-main">
                      <span className="mcp-server-name">{r.name}</span>
                      <span className="mcp-meta">{r.file}</span>
                      <span className={`mcp-status mcp-status-${r.state === 'approved' ? 'ready' : 'inactive'}`}>
                        {r.state === 'approved'
                          ? 'Approved'
                          : r.state === 'reserved'
                            ? 'Named like Helm’s own server, so it never loads.'
                            : 'Held back: it loads once you approve it.'}
                      </span>
                    </div>
                    {r.state === 'held' && (
                      <Button className="mcp-act" disabled={busy} onClick={() => ops.repoApprove(r).then(setRepo).catch((e: unknown) => setError(errorText(e)))}>
                        Approve
                      </Button>
                    )}
                    {r.state === 'approved' && (
                      <Button className="mcp-act" disabled={busy} onClick={() => ops.repoRevoke(r).then(setRepo).catch((e: unknown) => setError(errorText(e)))}>
                        Revoke
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </section>
      <section aria-labelledby="mcp-presets" data-testid="mcp-presets">
        <h3 id="mcp-presets">Add a server</h3>
        <div className="mcp-actions">
          {presets.map((p) => (
            <Button key={p.id} data-testid={`mcp-add-preset-${p.id}`} onClick={() => openPreset(p)}>
              {p.name}
            </Button>
          ))}
          <Button data-testid="mcp-add-custom" onClick={() => setDraft({ server: blankServer(ids), isNew: true })}>
            Other server…
          </Button>
        </div>
      </section>
    </div>
  );
}
