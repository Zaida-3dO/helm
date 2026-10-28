// The Tasks view: a status line, the project rail, four columns of cards and the item drawer. It
// is read-only. It reads when it is shown, when the window comes back into focus, every minute
// while it is shown, and on Refresh; nothing is read while it is hidden.

import { useEffect, useRef, useSyncExternalStore } from 'react';
import { BOARD_COLUMNS, COLUMN_TITLE, type BoardColumnId, type BoardEntry } from '../api/board.ts';
import { useShell } from '../shell/context.ts';
import { Button, Checkbox } from '../ui/index.ts';
import { ItemDrawer } from './ItemDrawer.tsx';
import { columnCount, holdersFor, shortId } from './model.ts';
import { tasksStoreFor } from './open.ts';
import { REFRESH_EVERY_MS, visibleProjects, type TasksState, type TasksStore } from './store.ts';

/** Reads again at most this often when the window is focused repeatedly. */
const FOCUS_REFRESH_GAP_MS = 5_000;

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function statusLine(s: TasksState): string {
  if (s.connection === 'missing') return 'Add the board address and token in Settings, Projects to see tasks.';
  if (!s.loaded) return s.error ?? 'Reading board…';
  const parts = ['Updates when this tab is shown and every minute while it stays open'];
  if (s.lastReadAt !== null) parts.push(`Last complete read ${clock(s.lastReadAt)}`);
  parts.push(s.ready === null ? 'Board readiness unavailable' : s.ready ? 'Board readiness confirmed' : 'Board says it is not ready');
  return parts.join(' · ');
}

export function TasksView({ visible, focused }: { visible: boolean; focused: boolean }) {
  const shell = useShell();
  const store = tasksStoreFor(shell.api);
  const s = useSyncExternalStore(store.subscribe, store.snapshot);
  const lastFocusRead = useRef(0);

  // Read on show; stop when hidden. A missing connection reads nothing.
  useEffect(() => {
    if (!visible) {
      store.pause();
      return;
    }
    let live = true;
    void store.checkConnection().then((ok) => {
      if (live && ok) void store.refresh();
    });
    const timer = setInterval(() => {
      if (store.snapshot().connection === 'ready' && !document.hidden) void store.refresh();
    }, REFRESH_EVERY_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [visible, store]);

  // Coming back to the window while the tab is shown reads again (the show above already read).
  const before = useRef({ visible: false, focused: false });
  useEffect(() => {
    const was = before.current;
    before.current = { visible, focused };
    if (!(visible && focused && was.visible && !was.focused)) return;
    const now = Date.now();
    if (now - lastFocusRead.current < FOCUS_REFRESH_GAP_MS) return;
    lastFocusRead.current = now;
    if (store.snapshot().connection === 'ready') void store.refresh();
  }, [visible, focused, store]);

  return (
    <div className="tk-root" data-testid="tasks-view" data-connection={s.connection} data-loaded={s.loaded ? 'true' : 'false'}>
      <div className="tk-status" data-testid="tasks-status" role="status">
        {statusLine(s)}
      </div>
      {s.stale && s.loaded && (
        <div className="tk-stale" data-testid="tasks-stale" role="alert">
          <span>{s.error ? `${s.error} ` : ''}What shows may be out of date.</span>
          <Button variant="text" onClick={() => void store.refresh()}>
            Refresh
          </Button>
        </div>
      )}
      {!s.loaded && s.error && s.connection !== 'missing' && (
        <div className="tk-stale" role="alert" data-testid="tasks-error">
          <span>{s.error}</span>
          <Button variant="text" onClick={() => void store.refresh()}>
            Retry
          </Button>
        </div>
      )}
      <div className="tk-body">
        <Rail store={store} s={s} />
        <div className="tk-board" data-testid="tasks-board">
          {BOARD_COLUMNS.map((c) => (
            <Column key={c} store={store} s={s} col={c} />
          ))}
        </div>
        {s.drawerId && <ItemDrawer store={store} key={s.drawerId} />}
      </div>
      <div className="tk-foot">You can browse the board here but not change it. Columns and holders are read separately.</div>
    </div>
  );
}

function Rail({ store, s }: { store: TasksStore; s: TasksState }) {
  const rows = visibleProjects(s);
  const shown = rows.slice(0, s.projects.shown);
  return (
    <nav className="tk-rail" aria-label="Projects" data-testid="tasks-rail">
      <button type="button" className="tk-proj" data-active={s.projects.selected === null} data-testid="tasks-project-all" onClick={() => store.selectProject(null)}>
        All projects
      </button>
      <Checkbox className="tk-check" label="Completed projects" checked={s.projects.includeCompleted} onChange={(on) => store.setIncludeCompleted(on)} />
      {shown.map((p) => (
        <button key={p.id} type="button" className="tk-proj" data-active={s.projects.selected === p.id} data-testid="tasks-project" data-project-id={p.id} onClick={() => store.selectProject(p.id)}>
          {p.title}
        </button>
      ))}
      {rows.length > shown.length && (
        <Button variant="text" onClick={() => store.showMoreProjects()}>
          Load more projects
        </Button>
      )}
      {s.projects.error && (
        <div className="tk-error" role="alert">
          {s.projects.error}
        </div>
      )}
    </nav>
  );
}

function Column({ store, s, col }: { store: TasksStore; s: TasksState; col: BoardColumnId }) {
  const c = s.columns[col];
  return (
    <section className="tk-col" data-testid="tasks-column" data-column={col} aria-label={COLUMN_TITLE[col]}>
      <header className="tk-col-head">
        <h3>{COLUMN_TITLE[col]}</h3>
        <span className="tk-count" data-testid="tasks-column-count">
          {columnCount(c.entries.length, c.total, c.started)}
        </span>
      </header>
      {c.entries.map((e) => (
        <Card key={e.item.id} store={store} s={s} entry={e} />
      ))}
      {c.entries.length === 0 && !c.loading && !c.error && c.started && <div className="tk-muted">No tasks</div>}
      {c.loading && <div className="tk-muted">Loading…</div>}
      {c.error && (
        <div className="tk-error" role="alert">
          {c.error}
        </div>
      )}
      {c.nextCursor && !c.loading && (
        <Button variant="text" data-testid="tasks-column-more" onClick={() => void store.loadMore(col)}>
          Load more
        </Button>
      )}
      {c.error && !c.nextCursor && !c.loading && c.entries.length === 0 && (
        <Button variant="text" onClick={() => void store.refresh()}>
          Retry column
        </Button>
      )}
    </section>
  );
}

function Card({ store, s, entry }: { store: TasksStore; s: TasksState; entry: BoardEntry }) {
  const ref = useRef<HTMLButtonElement>(null);
  const id = entry.item.id;
  // The checkpoint headline is read when the card scrolls into view, once per snapshot.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (typeof IntersectionObserver === 'undefined') {
      void store.loadHeadline(id);
      return;
    }
    const io = new IntersectionObserver((hits) => {
      if (hits.some((h) => h.isIntersecting)) {
        void store.loadHeadline(id);
        io.disconnect();
      }
    });
    io.observe(el);
    return () => io.disconnect();
  }, [store, id, s.revision]);
  const holders = holdersFor(entry, s.fleet);
  const head = s.headlines[id];
  const headline = head && head.revision === s.revision ? head : null;
  return (
    <button
      ref={ref}
      type="button"
      className="tk-card"
      data-testid="tasks-card"
      data-item-id={id}
      data-selected={s.drawerId === id}
      onClick={() => store.openDrawer(id)}
    >
      <span className="tk-card-title">{entry.item.title}</span>
      <span className="tk-meta">
        {entry.item.state}
        {entry.item.priority ? ` · ${entry.item.priority}` : ''} · {shortId(id)}
      </span>
      {holders.kind === 'lines' && holders.lines.map((l) => <span key={l} className="tk-holder">{l}</span>)}
      {holders.kind === 'unavailable' && <span className="tk-meta">Holders unavailable</span>}
      {holders.kind === 'none' && <span className="tk-meta">No live holders in the fleet snapshot</span>}
      {headline && (headline.failed || headline.text) && (
        <span className="tk-checkpoint" data-testid="tasks-card-checkpoint">
          {headline.failed ? <span className="tk-meta">Checkpoint unavailable</span> : headline.text}
        </span>
      )}
    </button>
  );
}
