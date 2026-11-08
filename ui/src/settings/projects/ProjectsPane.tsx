// Settings → Projects: the Agent Standup connection, the default board, the boards (where chats
// are shown) and the folders (where chats run), each with archive and restore. The token field is
// write-only: the pane only ever learns "set" or "not set".

import { useEffect, useMemo, useState } from 'react';
import { projectOps, type BoardConnection, type BoardSyncState, type ProjectNode } from '../../api/projects.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Picker, type PickerOption } from '../../ui/index.ts';
import { getDefaultBoard, NO_DEFAULT_BOARD, setDefaultBoard, type DefaultBoardState } from '../../newchat/boards.ts';
import { openAddFolder, openNewProject, openPrimaryFolder } from '../../newchat/index.ts';
import { needsFolder, pickerProjects, primaryFolder } from '../../newchat/places.ts';

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

/** The default board picker's "let the host choose" value. */
const AUTOMATIC = '__auto__';

export function ProjectsPane() {
  const shell = useShell();
  const ops = useMemo(() => projectOps(shell.api), [shell.api]);
  const [projects, setProjects] = useState<ProjectNode[]>([]);
  const [conn, setConn] = useState<BoardConnection | null>(null);
  const [url, setUrl] = useState('');
  const [token, setToken] = useState('');
  const [sync, setSync] = useState<BoardSyncState | null>(null);
  const [boards, setBoards] = useState<DefaultBoardState>(NO_DEFAULT_BOARD);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    ops
      .list(true)
      .then(setProjects)
      .catch((e: unknown) => setError(errorText(e)));
    void getDefaultBoard(shell.api).then(setBoards);
  };

  useEffect(() => {
    let live = true;
    const load = () => {
      ops
        .list(true)
        .then((p) => live && setProjects(p))
        .catch((e: unknown) => setError(errorText(e)));
      void getDefaultBoard(shell.api).then((b) => live && setBoards(b));
    };
    load();
    ops
      .connection()
      .then((c) => {
        if (!live) return;
        setConn(c);
        setUrl(c.url);
      })
      .catch((e: unknown) => setError(errorText(e)));
    const offs = [
      shell.bus.on('board_sync', (e) => {
        setSync(e.state as BoardSyncState);
        load();
      }),
      // A board made or given a folder from this pane's dialogs, or anywhere else.
      shell.bus.on('project', load),
    ];
    return () => {
      live = false;
      offs.forEach((o) => o());
    };
  }, [ops, shell.api, shell.bus]);

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
      reload();
    } catch (e) {
      setError(errorText(e));
    }
    setBusy(false);
  };

  const toggleArchive = async (p: ProjectNode) => {
    try {
      await (p.archived ? ops.restore(p.id) : ops.archive(p.id));
      reload();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const chooseDefault = async (v: string) => {
    setError(null);
    try {
      setBoards(await setDefaultBoard(shell.api, v === AUTOMATIC ? null : v));
    } catch (e) {
      setError(errorText(e));
    }
  };

  const boardList = projects.filter((p) => p.kind === 'board');
  const folderList = projects.filter((p) => p.kind !== 'board');
  const fallbackName = boards.chosen ? null : boards.name;
  const defaultOptions: PickerOption<string>[] = [
    { value: AUTOMATIC, title: 'Automatic', subtitle: fallbackName ? `Now ${fallbackName}` : 'No board found yet' },
    ...pickerProjects(projects, true, boards.chosen, boards.hiddenBoardIds).map((p) => ({ value: p.id, title: p.name, text: p.name })),
  ];
  const defaultShown = boards.chosen ? (projects.find((p) => p.id === boards.chosen)?.name ?? boards.name ?? 'A board') : `Automatic${fallbackName ? ` (${fallbackName})` : ''}`;

  const shownError = sync?.error ?? error;
  return (
    <div className="sp-pane">
      <section className="sp-section" aria-labelledby="sp-board">
        <h3 id="sp-board">Agent Standup</h3>
        <p className="sp-help">
          Projects on the task board are read from Agent Standup and are where chats are shown. Renames, icons, colours and
          nesting stay in Helm; New project makes a project on the board.
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
          <button
            type="button"
            data-testid="board-refresh"
            title="Fetch the board's projects now"
            disabled={busy || !conn?.hasToken}
            onClick={() => void test()}
          >
            Refresh
          </button>
        </div>
        <p className="sp-help" data-testid="board-last-sync">
          {sync?.lastSyncAt
            ? `Last sync ${new Date(sync.lastSyncAt).toLocaleString()}, ${sync.projectCount} projects`
            : 'Not synced yet'}
        </p>
        {shownError && <p className="sp-error">{shownError}</p>}
      </section>
      <section className="sp-section" aria-labelledby="sp-default">
        <h3 id="sp-default">Default board</h3>
        <p className="sp-help">
          Where a chat with no project of its own goes: a quick question, a message from your phone, a scheduled run.
        </p>
        <Picker
          value={boards.chosen ?? AUTOMATIC}
          options={defaultOptions}
          onChange={(v) => void chooseDefault(v)}
          label={`Default board: ${defaultShown}`}
          display={defaultShown}
          heading="Default board"
          searchable
          settings
          testId="default-board"
        />
      </section>
      <section className="sp-section" aria-labelledby="sp-projects">
        <h3 id="sp-projects">Boards</h3>
        <p className="sp-help">Where chats are shown. Each board has a folder its new chats start in.</p>
        <div className="sp-actions">
          <Button onClick={() => openNewProject({ noChat: true })} data-testid="settings-new-project">
            New project…
          </Button>
        </div>
        <ul className="sp-list" data-testid="settings-boards">
          {boardList.map((p) => (
            <li key={p.id} className="sp-project" data-project-id={p.id}>
              <span className="sp-name">
                {p.name}
                <span className="sp-meta">
                  {` · ${primaryFolder(p, projects)?.folder ?? 'no folder yet'}`}
                  {p.board?.stale ? ' · not on the board' : ''}
                  {p.archived ? ' · archived' : ''}
                </span>
              </span>
              <span className="sp-actions">
                {!p.archived && !p.board?.stale && (
                  <button type="button" data-testid={needsFolder(p, projects) ? 'settings-set-folder' : undefined} onClick={() => openPrimaryFolder(p.id)}>
                    {needsFolder(p, projects) ? 'Set folder' : 'Change folder'}
                  </button>
                )}
                <button type="button" onClick={() => void toggleArchive(p)}>
                  {p.archived ? 'Restore' : 'Archive'}
                </button>
              </span>
            </li>
          ))}
        </ul>
      </section>
      <section className="sp-section" aria-labelledby="sp-folders">
        <h3 id="sp-folders">Folders</h3>
        <p className="sp-help">Where chats run. A folder is not shown in the sidebar; pick a board to show a chat under.</p>
        <div className="sp-actions">
          <Button onClick={() => openAddFolder()} data-testid="settings-add-folder">
            Add folder…
          </Button>
        </div>
        <ul className="sp-list" data-testid="settings-folders">
          {folderList.map((p) => (
            <li key={p.id} className="sp-project" data-project-id={p.id}>
              <span className="sp-name">
                {p.name}
                <span className="sp-meta">
                  {p.path ? ` · ${p.path}` : ' · local group'}
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
