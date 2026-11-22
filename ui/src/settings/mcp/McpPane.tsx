// Settings → MCP. The server list with each
// server's live status and readiness, an editor, and presets. Secrets are write-only: the pane
// only ever sees a saved secret's id, never its value.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { mcpOps, type BrowserInstall, type DoctorOutcome, type McpOps, type McpPreset, type McpServer, type RefKind, type RepoServer, type ServerStatus, type TestOutcome, type ValueRef } from '../../api/mcp.ts';
import { shellOps } from '../../api/shell.ts';
import { deviceOps, type DeviceInfo, type DeviceOps } from '../../devices/api.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Dialog, Segmented, TextArea, TextInput, toast } from '../../ui/index.ts';
import { IconCheck, IconWarning } from '../../ui/icons.tsx';
import { WheelMark } from '../../ui/status/index.ts';
import { GoogleAccountsDialog } from './GoogleAccounts.tsx';
import { ProfileValues } from './ProfileValues.tsx';
import { GOOGLE_PRESET, googleOps, isGoogleServer } from './google.ts';
import { WAITING_NOTE, canDisconnect, connectLabel, isOAuthServer, oauthOps, type OAuthStatus } from './oauth.ts';
import { BROKER_NAME_KEY, BROKER_PRESET, blankServer, brokerPathKey, isBrokerPathRow, renameBrokerBrowser, copyImportable, draftFromPreset, isBrokerServer, isPlainRemoteHttp, pageReadiness, serverReadiness, toSaveable } from './readiness.ts';

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
          <div className="mcp-ref-block" key={i}>
          <div className="mcp-ref" data-testid={`mcp-ref-${k}`}>
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
          {r.kind === 'literal' && k.trim() ? <ProfileValues name={k} value={r} onChange={(next) => set(k, k, next)} /> : null}
          </div>
        ) : null,
      )}
      <Button variant="text" onClick={() => onChange({ ...refs, '': { kind: 'literal', value: '' } })}>
        Add {kindLabel.toLowerCase()}
      </Button>
    </div>
  );
}

// ── The browser preset's "Your browser" block ──────────────────────────────────────────────
//
// The rows the broker reads are the whole of the server's point, so the editor shows them as
// fields instead of raw environment rows. They save as the same refs as always: the name,
// extension id and sites as plain literal values the store keeps sealed, the connect token as a
// secret. The browser's program row is named after the browser (upper-cased, dashes written as
// underscores, the way the broker builds it), and renaming the browser renames that row.

const EXTENSION_KEY = 'BROKER_ATTACHED_EXTENSION_ID';
const SITES_KEY = 'BROKER_ATTACHED_ACT_SITES';
const TOKEN_KEY = 'BROKER_ATTACHED_TOKEN';

/** The rows the block below already edits, left out of the generic environment list so the
 * same value never has two boxes. The connect token stays a generic row: there is where its
 * per-profile values live. */
function isBrokerFormRow(k: string): boolean {
  return k === BROKER_NAME_KEY || k === EXTENSION_KEY || k === SITES_KEY || isBrokerPathRow(k);
}

function BrokerBlock({ env, onChange, onFindBrowsers }: { env: Refs; onChange: (env: Refs) => void; onFindBrowsers?: () => Promise<BrowserInstall[]> }) {
  const [found, setFound] = useState<BrowserInstall[] | 'loading' | null>(null);
  const name = env[BROKER_NAME_KEY]?.value ?? '';
  const exact = brokerPathKey(name || 'brave');
  const pathKey = env[exact] || !Object.keys(env).some(isBrokerPathRow) ? exact : Object.keys(env).find(isBrokerPathRow)!;
  const put = (key: string, value: string) => onChange({ ...env, [key]: { kind: 'literal', value } });
  const setName = (raw: string) => onChange(renameBrokerBrowser(env, name, raw));
  // A saved value comes back sealed: the field is empty, so say it is kept, as the token field does.
  const kept = (key: string, fallback: string) => (env[key]?.kind === 'literal' && env[key]?.secretId && !env[key]?.value ? 'Saved; type to replace' : fallback);
  const find = () => {
    if (!onFindBrowsers) return;
    setFound('loading');
    onFindBrowsers()
      .then(setFound)
      .catch(() => setFound([]));
  };
  const token = env[TOKEN_KEY];
  return (
    <div className="mcp-browser">
      <label className="mcp-field">
        <span>Browser name</span>
        <TextInput data-testid="mcp-browser-name" spellCheck={false} value={name} placeholder={kept(BROKER_NAME_KEY, 'brave')} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="mcp-field">
        <span>Browser program</span>
        <TextInput data-testid="mcp-browser-program" spellCheck={false} value={env[pathKey]?.value ?? ''} placeholder={kept(pathKey, 'C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe')} onChange={(e) => put(pathKey, e.target.value)} />
      </label>
      <p className="mcp-help">
        Saved as <code>{pathKey}</code>; renaming the browser renames that variable.
      </p>
      <div className="mcp-actions">
        <Button data-testid="mcp-browser-find" variant="text" disabled={found === 'loading' || !onFindBrowsers} onClick={find}>
          {found === 'loading' ? 'Looking…' : 'Find installed browsers'}
        </Button>
      </div>
      {Array.isArray(found) && found.length === 0 && <p className="mcp-help">No installed browser in the usual places; type the path.</p>}
      {Array.isArray(found) && found.length > 0 && (
        <div className="mcp-actions">
          {found.map((b) => (
            <Button key={b.path} variant="text" onClick={() => put(pathKey, b.path)}>
              {b.label}
            </Button>
          ))}
        </div>
      )}
      <label className="mcp-field">
        <span>Extension id</span>
        <TextInput data-testid="mcp-browser-extension" spellCheck={false} autoComplete="off" value={env[EXTENSION_KEY]?.value ?? ''} placeholder={kept(EXTENSION_KEY, "32 letters from a to p, from your browser's extension page")} onChange={(e) => put(EXTENSION_KEY, e.target.value)} />
      </label>
      <label className="mcp-field">
        <span>Sites chats may act on</span>
        <TextInput data-testid="mcp-browser-sites" spellCheck={false} value={env[SITES_KEY]?.value ?? ''} placeholder={kept(SITES_KEY, 'shop.example, *.read.example.org')} onChange={(e) => put(SITES_KEY, e.target.value)} />
      </label>
      <p className="mcp-help">Empty means chats only read pages; they never act anywhere. Reading is allowed on any site.</p>
      <label className="mcp-field">
        <span>Connect token</span>
        <TextInput
          type="password"
          spellCheck={false}
          autoComplete="off"
          value={token?.kind === 'literal' ? token.value ?? '' : ''}
          placeholder={token?.kind === 'literal' && token.secretId ? 'Saved; type to replace' : 'Optional — only skips the connect page when the broker reconnects'}
          onChange={(e) => onChange({ ...env, [TOKEN_KEY]: { kind: 'literal', value: e.target.value } })}
        />
      </label>
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
  onFindBrowsers,
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
  /** The browser preset's Find button: what the host op sees installed on this PC. */
  onFindBrowsers?: () => Promise<BrowserInstall[]>;
}) {
  const t = draft.transport;
  const setT = (patch: Partial<McpServer['transport']>) => onChange({ ...draft, transport: { ...t, ...patch } });
  const remote = t.type !== 'stdio';
  // The browser preset gets the "Your browser" block; the generic environment list then hides
  // exactly the rows it writes (the connect token excepted: its per-profile values live there).
  const isBrokerDraft = draft.preset === BROKER_PRESET && t.type === 'stdio';
  const allEnv = t.env ?? {};
  const hiddenByBlock = (k: string) => isBrokerDraft && isBrokerFormRow(k);
  const shownEnv = Object.fromEntries(Object.entries(allEnv).filter(([k]) => !hiddenByBlock(k)));
  const setShownEnv = (visible: Refs) => {
    const kept = Object.fromEntries(Object.entries(allEnv).filter(([k]) => hiddenByBlock(k)));
    setT({ env: { ...kept, ...visible } });
  };
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
    <div className="mcp-editor" ref={box} data-testid="mcp-editor">
      <h4>{isNew ? 'Add a server' : `Edit ${draft.name}`}</h4>
      {preset && <p className="mcp-help">{preset.description}</p>}
      {preset?.id === GOOGLE_PRESET ? (
        <p className="mcp-help">
          Gmail is served by Helm itself: your sign-in client and accounts are set in the Google accounts dialog, not here. Leave Environment empty unless
          this server is an older one that carries its own client.
        </p>
      ) : preset?.id === BROKER_PRESET ? (
        <p className="mcp-help">
          The Your browser block sets which browser chats use, the broker extension's id in it, the sites chats may act on, and its optional connect
          token. Chats never change any of these, and never act on a site that is not listed.
        </p>
      ) : preset?.secretEnv?.length ? (
        <p className="mcp-help">Under Environment, enter {preset.secretEnv.join(' and ')} as secrets.</p>
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
                  onChange={(e) =>
                    // The saved secret's id stays while the box is edited: a typed value replaces it
                    // at save, and clearing the box again keeps the saved one.
                    setT({
                      oauth: {
                        ...t.oauth,
                        clientSecret: { kind: 'literal', value: e.target.value, secretId: t.oauth?.clientSecret?.kind === 'literal' ? t.oauth.clientSecret.secretId : undefined },
                      },
                    })
                  }
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
          {isBrokerDraft && <BrokerBlock env={allEnv} onChange={(env) => setT({ env })} onFindBrowsers={onFindBrowsers} />}
          <div className="mcp-field mcp-field-top">
            <span>Environment</span>
            <RefRows refs={shownEnv} kindLabel="Variable" onChange={setShownEnv} />
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
        <Button data-testid="mcp-cancel" onClick={onCancel}>
          Cancel
        </Button>
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

/** The "Copy from another PC" dialog: pick one of the person's own paired PCs, tick the tool
 * servers it has that this PC does not, and bring them over. Secrets crossed as empty slots —
 * every copied row waits for its token until the person enters it, and a server this PC already
 * has is left exactly as it is. */
function CopyDialog({
  ids,
  mcp,
  devices,
  onApply,
  onClose,
}: {
  ids: string[];
  mcp: McpOps;
  devices: DeviceOps;
  onApply: (snap: { servers: McpServer[]; revision: string }) => void;
  onClose: () => void;
}) {
  const [peers, setPeers] = useState<DeviceInfo[] | null>(null);
  const [device, setDevice] = useState<DeviceInfo | null>(null);
  const [offered, setOffered] = useState<McpServer[] | null>(null);
  const [skipped, setSkipped] = useState(0);
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    devices
      .list()
      .then((l) => setPeers(l.devices.filter((d) => !d.isSelf)))
      .catch((e: unknown) => {
        setPeers([]);
        setError(errorText(e));
      });
  }, [devices]);
  const choose = (d: DeviceInfo) => {
    setDevice(d);
    setOffered(null);
    setPicked([]);
    setError(null);
    devices
      .mcpList(d.uid)
      .then((r) => {
        const list = copyImportable(r.servers, ids);
        setSkipped(r.servers.length - list.length);
        setOffered(list);
      })
      .catch((e: unknown) => setError(errorText(e)));
  };
  const importPicked = () => {
    const chosen = (offered ?? []).filter((s) => picked.includes(s.id));
    if (chosen.length === 0) return;
    setBusy(true);
    mcp
      .importCopy(chosen)
      .then((snap) => {
        onApply(snap);
        onClose();
      })
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog
      title="Copy tool servers from another PC"
      testId="mcp-copy-dialog"
      busy={busy}
      onClose={onClose}
      intro={
        <p className="mcp-help">
          {device
            ? `${device.name}: its servers that this PC does not have yet. A secret is never copied — an imported row waits until you enter its token, and a server already here is left as it is.`
            : 'Pick one of your other PCs. Only a PC of yours answers; its secrets stay there.'}
        </p>
      }
      footer={
        <>
          <Button variant="primary" data-testid="mcp-copy-import" disabled={busy || picked.length === 0} onClick={importPicked}>
            {busy ? 'Copying…' : picked.length ? `Copy ${picked.length}` : 'Copy'}
          </Button>
          <Button data-testid="mcp-copy-close" onClick={onClose}>
            Close
          </Button>
        </>
      }
    >
      {peers === null && <p className="mcp-help">Loading your PCs…</p>}
      {peers !== null && peers.length === 0 && <p className="mcp-help">No other PC shares your hub, so there is nothing to copy from.</p>}
      <div className="mcp-actions">
        {(peers ?? []).map((d) => (
          <Button key={d.uid} data-testid={`mcp-copy-device-${d.uid}`} disabled={busy} onClick={() => choose(d)}>
            {d.name}
            {d.online ? '' : ' (offline)'}
          </Button>
        ))}
      </div>
      {device && offered === null && !error && <p className="mcp-help">Reading {device.name}’s server list…</p>}
      {offered !== null && (
        <>
          <ul className="mcp-list">
            {offered.map((s) => (
              <li key={s.id} className="mcp-server" data-testid={`mcp-copy-row-${s.id}`}>
                <div className="mcp-server-main">
                  <span className="mcp-server-name">{s.name}</span>
                  <span className="mcp-meta">{s.transport.type === 'stdio' ? s.transport.command : s.transport.url}</span>
                </div>
                <Checkbox
                  aria-label={`Copy ${s.name}`}
                  checked={picked.includes(s.id)}
                  disabled={busy}
                  onChange={(on) => setPicked((p) => (on ? [...p, s.id] : p.filter((x) => x !== s.id)))}
                  label="Copy"
                />
              </li>
            ))}
          </ul>
          {offered.length === 0 && (
            <p className="mcp-help" data-testid="mcp-copy-empty">
              Nothing to bring: every server there is already on this PC.
            </p>
          )}
          {skipped > 0 && (
            <p className="mcp-help">
              {skipped} {skipped === 1 ? 'is' : 'are'} already on this PC, left exactly as they are.
            </p>
          )}
        </>
      )}
      {error && (
        <p className="mcp-error" role="alert">
          {error}
        </p>
      )}
    </Dialog>
  );
}

export function McpPane() {
  const shell = useShell();
  const ops = useMemo(() => mcpOps(shell.api), [shell.api]);
  const google = useMemo(() => googleOps(shell.api), [shell.api]);
  const oauth = useMemo(() => oauthOps(shell.api), [shell.api]);
  const devices = useMemo(() => deviceOps(shell.api), [shell.api]);
  // The "Copy from another PC" dialog.
  const [copyOpen, setCopyOpen] = useState(false);
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
  // For the browser server: its last doctor run ('running' while it is open).
  const [doctor, setDoctor] = useState<Record<string, DoctorOutcome | 'running'>>({});

  const runDoctor = (id: string) => {
    setDoctor((d) => ({ ...d, [id]: 'running' }));
    ops
      .doctor(id)
      .then((o) => setDoctor((d) => ({ ...d, [id]: o })))
      .catch((e: unknown) => setDoctor((d) => ({ ...d, [id]: { ok: false, message: errorText(e), checks: [] } })));
  };

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

  /** Clicking a preset must open its setup visibly: a Google server goes straight to its dialog
   * (its draft has no choices to make) — and if one already exists, the preset opens that one
   * rather than saving a second entry. The rest go to the editor. */
  const openPreset = (p: McpPreset) => {
    if (p.id === GOOGLE_PRESET) {
      const existing = servers.find(isGoogleServer);
      if (existing) {
        setGoogleDialog(existing.id);
        return;
      }
      const server = draftFromPreset(p, ids);
      void run(() => ops.save(toSaveable(server), revision)).then((ok) => ok && setGoogleDialog(server.id));
      return;
    }
    setDraft({ server: draftFromPreset(p, ids), isNew: true });
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
            const doc = doctor[s.id];
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
                {isBrokerServer(s) && (
                  <Button className="mcp-act" data-testid={`mcp-doctor-${s.id}`} disabled={busy || doc === 'running'} onClick={() => runDoctor(s.id)}>
                    {doc === 'running' ? 'Checking…' : 'Check the browser'}
                  </Button>
                )}
                <Button className="mcp-act" data-testid={`mcp-edit-${s.id}`} onClick={() => setDraft({ server: structuredClone(s), isNew: false })}>
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
                {isBrokerServer(s) && doc && doc !== 'running' && (
                  <div className="mcp-doctor" data-testid={`mcp-doctor-result-${s.id}`} role="status">
                    <p className="mcp-test" data-state={doc.ok ? 'ok' : 'failed'}>
                      {doc.ok ? <IconCheck size={14} /> : <IconWarning size={14} />}
                      {doc.message}
                    </p>
                    {doc.checks && doc.checks.length > 0 && (
                      <ul className="mcp-doctor-checks">
                        {doc.checks.map((c) => (
                          <li key={c.id} data-testid={`mcp-doctor-${s.id}-${c.id}`} data-state={c.status}>
                            {c.title}: {c.detail}
                            {c.remedy ? ` — ${c.remedy}` : ''}
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
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
        {copyOpen && (
          <CopyDialog
            ids={ids}
            mcp={ops}
            devices={devices}
            onApply={(s) => {
              setServers(s.servers);
              setRevision(s.revision);
            }}
            onClose={() => setCopyOpen(false)}
          />
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
            onFindBrowsers={ops.findBrowsers}
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
          <Button data-testid="mcp-copy-remote" onClick={() => setCopyOpen(true)}>
            Copy from another PC…
          </Button>
        </div>
        <p className="mcp-help">
          Or bring the same servers you set up on another of your PCs. Their secrets never cross: each copied server waits for you to enter its token here.
        </p>
      </section>
    </div>
  );
}
