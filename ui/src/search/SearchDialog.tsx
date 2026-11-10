// The search dialog (Mod+K): one combobox over grouped sources: Chats and
// Files. Up and Down move across groups and wrap, PageUp and PageDown move ten rows,
// Enter opens the highlighted row, and Escape clears a typed query before it closes the dialog.
// Search is local and never calls a model.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { handleLink } from '../panel/index.ts';
import { useDialog, useShell } from '../shell/context.ts';
import { Checkbox, Dialog, Icon, Picker, useBinding, useLayer, type PickerOption } from '../ui/index.ts';
import { enterTarget, flatRows, moveHighlight, movePage, statusLine, UniversalSearch } from './controller.ts';
import { openTasks, tasksStoreFor } from '../tasks/index.ts';
import { openClaudeChatViewer } from '../claudechats/index.ts';
import { boardSource } from './boardSource.ts';
import { chatsSource, claudeChatsSource, filesSource } from './defaultSources.ts';
import { prsSource } from './prSource.ts';
import { openPrTab } from '../pr/route.ts';
import { GROUP_LABEL, groupOrder, type SourceId } from './sources.ts';
import type { ProjectNode } from '../api/projects.ts';
import { getDefaultBoard } from '../newchat/boards.ts';
import { pickerProjects } from '../newchat/places.ts';
import './search.css';

export const SEARCH_DIALOG = 'search';

export function SearchDialog() {
  const shell = useShell();
  const { open, close } = useDialog(SEARCH_DIALOG);

  // Ctrl+K opens it (and closes it again), through the shared key registry so it can be rebound.
  useBinding({
    id: 'search.toggle',
    title: 'Search',
    defaultKeys: 'Ctrl+K',
    run: () => (shell.openDialogId === SEARCH_DIALOG ? shell.closeDialog(SEARCH_DIALOG) : shell.openDialog(SEARCH_DIALOG)),
  });

  if (!open) return null;
  return <SearchBody close={close} />;
}

function SearchBody({ close }: { close: () => void }) {
  const shell = useShell();
  const [text, setText] = useState('');
  const [projectId, setProjectId] = useState<string | null>(null);
  const [archived, setArchived] = useState(false);
  const [projects, setProjects] = useState<ProjectNode[]>([]);
  const [hidden, setHidden] = useState<string[]>([]);
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  // Escape clears a typed query before it closes the dialog (the shared Escape stack: search, then dialog).
  useLayer('search', text !== '', () => setText(''));

  // Sources read the filters and the selection when a search runs, through these refs.
  const filters = useRef({ projectId, archived });
  filters.current = { projectId, archived };

  // `close` may be a new function on every render; the sources call it through a ref so the search
  // (and the query it is running) is not rebuilt each time.
  const closeRef = useRef(close);
  closeRef.current = close;
  const closeNow = useCallback(() => closeRef.current(), []);

  const availability = useRef<Promise<boolean> | null>(null);
  const search = useMemo(
    () =>
      new UniversalSearch([
        chatsSource({
          api: shell.api,
          filters: () => filters.current,
          currentProjectId: () => shell.selection.projectId,
          open: (hit) => {
            shell.select(hit.chatId, hit.messageId ? { messageId: hit.messageId } : {});
            closeNow();
          },
        }),
        claudeChatsSource({
          api: shell.api,
          open: (hit) => {
            openClaudeChatViewer(shell.openDialog, hit.sessionId);
            closeNow();
          },
        }),
        filesSource({
          api: shell.api,
          chatId: () => shell.selection.chatId,
          cwd: () => {
            const id = shell.selection.chatId;
            return id ? (shell.store.view(id).chat?.cwd ?? shell.store.chatList().find((c) => c.id === id)?.cwd ?? null) : null;
          },
          openFile: (path) => {
            void handleLink({ kind: 'url', href: path }, { chatId: shell.selection.chatId });
            closeNow();
          },
        }),
        boardSource({
          reader: tasksStoreFor(shell.api).reader,
          // One look at the saved connection per dialog, not one per keystroke.
          available: () => (availability.current ??= tasksStoreFor(shell.api).checkConnection()),
          openTasks: (itemId) => void openTasks(shell.api, itemId),
          close: closeNow,
        }),
        prsSource({
          api: shell.api,
          open: (ref) => {
            openPrTab({ ...ref, view: 'conversation' }, shell.selection.chatId);
            closeNow();
          },
        }),
      ]),
    [shell, closeNow],
  );
  useEffect(() => () => search.dispose(), [search]);
  const state = useSyncExternalStore(search.subscribe, search.snapshot);

  useEffect(() => {
    // The project list belongs to the sidebar; without it the filter is simply absent. Chats are
    // shown under boards, so the filter offers the boards a picker offers (finished ones too).
    shell
      .api<ProjectNode[]>('project_list', {})
      .then(setProjects)
      .catch(() => setProjects([]));
    void getDefaultBoard(shell.api).then((b) => setHidden(b.hiddenBoardIds));
  }, [shell]);

  useEffect(() => {
    search.update(text);
    setIndex(0);
  }, [search, text, projectId, archived]);

  const projectOptions = useMemo<PickerOption<string>[]>(
    () => [{ value: '', title: 'All projects' }, ...pickerProjects(projects, true, projectId, hidden).map((p) => ({ value: p.id, title: p.name }))],
    [projects, projectId, hidden],
  );
  const rows = flatRows(state);
  const current = rows[index];
  useEffect(() => {
    if (!current) return;
    document.getElementById(current.row.id)?.scrollIntoView({ block: 'nearest' });
  }, [current]);

  const onKey = (e: ReactKeyboardEvent) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      setIndex((i) => moveHighlight(i, e.key === 'ArrowDown' ? 1 : -1, rows.length));
    } else if (e.key === 'PageDown' || e.key === 'PageUp') {
      e.preventDefault();
      setIndex((i) => movePage(i, e.key === 'PageDown' ? 1 : -1, rows.length));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      enterTarget(state, index)?.open();
    }
  };

  const present = Object.keys(state.groups) as SourceId[];
  const order = groupOrder(state.query, present);
  const typed = state.query !== '';
  const nothing = typed && state.status === 'done' && rows.length === 0 && order.every((g) => state.groups[g]?.status !== 'error');
  return (
    <Dialog title="Search chats" wide onClose={close} testId="search-dialog">
      <div className="search-box">
        <div className="search-field">
          <span className="search-field-icon" aria-hidden="true">
            <Icon name="search" size={16} />
          </span>
          <input
            type="text"
            role="combobox"
            aria-expanded={rows.length > 0}
            aria-controls="search-results"
            aria-activedescendant={current?.row.id}
            aria-autocomplete="list"
            className="search-input"
            data-testid="search-input"
            placeholder={'Search chats and files…'}
            aria-label="Search"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKey}
            autoFocus
          />
          <kbd className="search-esc">esc</kbd>
        </div>
        <div className="search-filters">
          {projects.length > 0 && (
            <Picker
              className="search-project"
              label="Project"
              heading="Search in"
              searchable={projects.length > 8}
              searchPlaceholder="Find a project…"
              value={projectId ?? ''}
              options={projectOptions}
              onChange={(v) => setProjectId(v || null)}
              testId="search-project"
            />
          )}
          <Checkbox className="search-archived" checked={archived} onChange={setArchived} label="Archived chats" />
          <span className="search-status muted" role="status" aria-live="polite" data-testid="search-status" data-state={state.status}>
            {statusLine(state)}
          </span>
        </div>
      </div>
      {!typed && (
        <p className="search-empty muted">
          <span className="search-empty-icon" aria-hidden="true">
            <Icon name="search" size={22} />
          </span>
          Find something you worked on.
          <br />
          Search stays on this PC and never calls a model.
        </p>
      )}
      {nothing && <p className="search-empty muted">Nothing matches.</p>}
      <div id="search-results" role="listbox" aria-label="Results" className="search-results" ref={listRef} data-testid="search-results">
        {order.map((g) => {
          const group = state.groups[g];
          if (!group) return null;
          const hasRows = group.rows.length > 0;
          if (!hasRows && group.status !== 'error') return null;
          return (
            <div key={g} role="group" aria-labelledby={`search-group-${g}`} className="search-group" data-testid="search-group" data-group={g}>
              <div id={`search-group-${g}`} className="search-group-label">
                {GROUP_LABEL[g]}
              </div>
              {group.status === 'error' && (
                <div className="search-group-error error" role="alert">
                  {GROUP_LABEL[g]} could not be searched: {group.error}
                </div>
              )}
              {group.rows.map((row) => (
                <div
                  key={row.id}
                  id={row.id}
                  role="option"
                  aria-selected={row.id === current?.row.id}
                  className="search-row"
                  data-testid="search-hit"
                  data-group={g}
                  onMouseMove={() => setIndex(rows.findIndex((r) => r.row.id === row.id))}
                  onClick={() => row.open()}
                >
                  <div className="search-row-head">
                    <span className="search-row-title">{row.title}</span>
                    {row.tag && <span className="search-row-tag">{row.tag}</span>}
                    {row.subtitle && <span className="search-row-project muted">{row.subtitle}</span>}
                  </div>
                  {row.snippet && <div className="search-row-snippet">{row.snippet}</div>}
                </div>
              ))}
            </div>
          );
        })}
      </div>
    </Dialog>
  );
}
