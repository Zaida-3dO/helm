// Settings → Projects: the project list with archive and restore, and the Agent Standup
// connection. The token field is write-only: the pane only ever learns "set" or "not set".

import { useEffect, useMemo, useState } from 'react';
import { projectOps, type BoardConnection, type BoardSyncState, type ProjectNode } from '../../api/projects.ts';
import { useShell } from '../../shell/context.ts';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

export function ProjectsPane() {
  const shell = useShell();
  const ops = useMemo(() => projectOps(shell.api), [shell.api]);
  const [projects, setProjects] = useState<ProjectNode[]>([]);
  const [conn, setConn] = useState<BoardConnection | null>(null);
  const [url, setUrl] = useState('');
  const [token, setToken] = useState('');
  const [sync, setSync] = useState<BoardSyncState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    ops
      .list(true)
      .then((p) => live && setProjects(p))
      .catch((e: unknown) => setError(errorText(e)));
    ops
      .connection()
      .then((c) => {
        if (!live) return;
        setConn(c);
        setUrl(c.url);
      })
      .catch((e: unknown) => setError(errorText(e)));
    const off = shell.bus.on('board_sync', (e) => setSync(e.state as BoardSyncState));
    return () => {
      live = false;
      off();
    };
  }, [ops, shell.bus]);

  const reload = () =>
    ops
      .list(true)
      .then(setProjects)
      .catch((e: unknown) => setError(errorText(e)));

  const save = async (clearToken = false) => {
    setBusy(true);
    setError(null);
    try {
      const c = await ops.saveConnection(url, clearToken ? '' : token || undefined);
      setConn(c);
      setUrl(c.url);
      setToken('');
    } catch (e) {
      setError(errorText(e));
    }
    setBusy(false);
  };

  const test = async () => {
    setBusy(true);
    setError(null);
    try {
      setSync(await ops.sync());
      void reload();
    } catch (e) {
      setError(errorText(e));
    }
    setBusy(false);
  };

  const toggleArchive = async (p: ProjectNode) => {
    try {
      await (p.archived ? ops.restore(p.id) : ops.archive(p.id));
      void reload();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const shownError = sync?.error ?? error;
  return (
    <div className="sp-pane">
      <section className="sp-section" aria-labelledby="sp-board">
        <h3 id="sp-board">Agent Standup</h3>
        <p className="sp-help">
          Board projects are read from Agent Standup and shown as groups. Helm never writes to it; renames, icons, colours
          and nesting stay in Helm.
        </p>
        <label className="sp-row">
          <span>Address</span>
          <input
            data-testid="board-url"
            type="url"
            value={url}
            placeholder="https://standup.example.com"
            spellCheck={false}
            onChange={(e) => setUrl(e.target.value)}
          />
          <span />
        </label>
        <label className="sp-row">
          <span>Token</span>
          <input
            data-testid="board-token"
            type="password"
            autoComplete="off"
            value={token}
            placeholder={conn?.hasToken ? 'Set; enter a new one to replace it' : 'Not set'}
            onChange={(e) => setToken(e.target.value)}
          />
          <span className="sp-state" data-testid="board-token-state">
            {conn?.hasToken ? 'set' : 'not set'}
          </span>
        </label>
        <div className="sp-actions">
          <button type="button" disabled={busy} onClick={() => void save()}>
            Save
          </button>
          <button type="button" disabled={busy || !conn?.hasToken} onClick={() => void save(true)}>
            Remove token
          </button>
          <button type="button" data-testid="board-test" disabled={busy} onClick={() => void test()}>
            Test connection
          </button>
        </div>
        <p className="sp-help" data-testid="board-last-sync">
          {sync?.lastSyncAt
            ? `Last sync ${new Date(sync.lastSyncAt).toLocaleString()}, ${sync.projectCount} projects`
            : 'Not synced yet'}
        </p>
        {shownError && <p className="sp-error">{shownError}</p>}
      </section>
      <section className="sp-section" aria-labelledby="sp-projects">
        <h3 id="sp-projects">Projects</h3>
        <ul className="sp-list">
          {projects.map((p) => (
            <li key={p.id} className="sp-project" data-project-id={p.id}>
              <span className="sp-name">
                {p.name}
                <span className="sp-meta">
                  {p.kind === 'board' ? ' · Agent Standup' : p.path ? ` · ${p.path}` : ' · local group'}
                  {p.board?.stale ? ' · not on the board' : ''}
                  {p.archived ? ' · archived' : ''}
                </span>
              </span>
              <button type="button" onClick={() => void toggleArchive(p)}>
                {p.archived ? 'Restore' : 'Archive'}
              </button>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
