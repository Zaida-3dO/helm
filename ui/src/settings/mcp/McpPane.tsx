// Settings → MCP. The server list with each
// server's live status and readiness, an editor, and presets. Secrets are write-only: the pane
// only ever sees a saved secret's id, never its value.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { mcpOps, type McpPreset, type McpServer, type RefKind, type RepoServer, type ServerStatus, type ValueRef } from '../../api/mcp.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, Segmented, TextArea, TextInput } from '../../ui/index.ts';
import { blankServer, draftFromPreset, isPlainRemoteHttp, pageReadiness, serverReadiness, toSaveable } from './readiness.ts';

type Refs = { [key in string]?: ValueRef };

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

function RefRows({ refs, onChange, kindLabel }: { refs: Refs; onChange: (r: Refs) => void; kindLabel: string }) {
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
              options={[
                { value: 'literal', label: 'Secret' },
                { value: 'environment', label: 'Env var' },
                { value: 'file', label: 'File' },
              ]}
              onChange={(kind) => set(k, k, { kind, value: '', ...(r.prefix ? { prefix: r.prefix } : {}) })}
            />
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
  preset,
}: {
  draft: McpServer;
  isNew: boolean;
  busy: boolean;
  onChange: (d: McpServer) => void;
  onSave: () => void;
  onCancel: () => void;
  preset?: McpPreset;
}) {
  const t = draft.transport;
  const setT = (patch: Partial<McpServer['transport']>) => onChange({ ...draft, transport: { ...t, ...patch } });
  const remote = t.type !== 'stdio';
  return (
    <div className="mcp-editor" data-testid="mcp-editor">
      <h4>{isNew ? 'Add a server' : `Edit ${draft.name}`}</h4>
      {preset && <p className="mcp-help">{preset.description}</p>}
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
            <RefRows refs={t.headers ?? {}} kindLabel="Header" onChange={(headers) => setT({ headers })} />
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
        <Button onClick={onCancel}>Cancel</Button>
      </div>
    </div>
  );
}

export function McpPane() {
  const shell = useShell();
  const ops = useMemo(() => mcpOps(shell.api), [shell.api]);
  const [servers, setServers] = useState<McpServer[]>([]);
  const [revision, setRevision] = useState<string | undefined>();
  const [presets, setPresets] = useState<McpPreset[]>([]);
  const [status, setStatus] = useState<Record<string, ServerStatus>>({});
  const [draft, setDraft] = useState<{ server: McpServer; isNew: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [native, setNative] = useState(true);
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

  useEffect(() => {
    reload();
    reloadStatus();
    ops.nativeOn().then(setNative).catch(() => {});
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
  const empty = pageReadiness(servers);
  return (
    <div className="mcp-pane" data-testid="mcp-pane">
      <section aria-labelledby="mcp-servers">
        <h3 id="mcp-servers">MCP servers</h3>
        <p className="mcp-help">
          Servers Helm connects chats to through its own client. When Claude Code has its own copy of a server listed here, chats use
          Helm's copy.
        </p>
        {empty && (
          <div className="mcp-empty" data-testid="mcp-empty" data-state={empty.state}>
            {empty.reason}
          </div>
        )}
        <ul className="mcp-list">
          {servers.map((s) => {
            const r = serverReadiness(s, status[s.id], presets);
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
                <Button variant="text" onClick={() => setDraft({ server: structuredClone(s), isNew: false })}>
                  Edit
                </Button>
                <Button variant="text" danger disabled={busy} onClick={() => void run(() => ops.remove(s.id, revision))}>
                  Remove
                </Button>
              </li>
            );
          })}
        </ul>
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
            ? 'Chats also load the servers Claude Code loads for you: your own, the claude.ai connectors and approved repository servers. Helm skips its own copy of any server Claude Code already loads.'
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
                      <Button disabled={busy} onClick={() => ops.repoApprove(r).then(setRepo).catch((e: unknown) => setError(errorText(e)))}>
                        Approve
                      </Button>
                    )}
                    {r.state === 'approved' && (
                      <Button variant="text" danger disabled={busy} onClick={() => ops.repoRevoke(r).then(setRepo).catch((e: unknown) => setError(errorText(e)))}>
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
            <Button key={p.id} data-testid={`mcp-add-preset-${p.id}`} onClick={() => setDraft({ server: draftFromPreset(p, ids), isNew: true })}>
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
