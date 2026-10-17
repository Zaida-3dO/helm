// The terminal dock: a bottom dock in the main pane with session tabs,
// New / Find / Maximise / Hide, a drag-to-resize top edge (double-click maximises) and a remembered
// height. Ctrl+` and Ctrl+J toggle it; inside, the dock's own commands are Ctrl+Shift chords so
// Ctrl+C and friends always reach the shell.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { shellOps } from '../api/shell.ts';
import { terminalOps } from '../api/terminal.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, toast, useBinding, useLayer } from '../ui/index.ts';
import { clampHeight, HEIGHT_DEFAULT, nextTabState, SESSIONS_MAX, type DockTab } from './model.ts';
import './terminal.css';
import { XtermView, type XtermHandle } from './XtermView.tsx';

export function TerminalDock() {
  const shell = useShell();
  const { chatId } = useSelection();
  const ops = useMemo(() => terminalOps(shell.api), [shell]);
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [tabs, setTabs] = useState<DockTab[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [height, setHeight] = useState(HEIGHT_DEFAULT);
  const [max, setMax] = useState(false);
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState('');
  const views = useRef(new Map<string, XtermHandle>());
  const starting = useRef(false);
  const dock = useRef<HTMLDivElement>(null);
  const chat = useSyncExternalStore(shell.store.subscribe, () =>
    chatId ? (shell.store.view(chatId).chat ?? shell.store.chatList().find((c) => c.id === chatId) ?? null) : null,
  );

  useEffect(() => {
    shell
      .api<unknown>('preference_get', { key: 'terminalHeight' })
      .then((v) => typeof v === 'number' && setHeight(v))
      .catch(() => undefined);
  }, [shell]);

  const newSession = async (from?: DockTab) => {
    if (tabs.length >= SESSIONS_MAX) {
      toast.show({ kind: 'error', text: `At most ${SESSIONS_MAX} terminals can be open; close one first.` });
      return;
    }
    try {
      const s = await ops.open({ cwd: from?.cwd ?? chat?.cwd ?? undefined, cols: 100, rows: 24 });
      setTabs((t) => [...t, { id: s.id, title: s.title, cwd: s.cwd, state: 'idle' }]);
      setActive(s.id);
    } catch (e) {
      toast.show({ kind: 'error', text: String((e as Error).message ?? e) });
    }
  };

  const toggle = () => {
    setLoaded(true);
    const next = !open;
    setOpen(next);
    if (next && tabs.length === 0 && !starting.current) {
      starting.current = true;
      // Reattach to sessions that survived a renderer reload, else start one.
      void ops
        .sessions()
        .then((list) => {
          if (list.length) {
            setTabs(list.map((s) => ({ id: s.id, title: s.title, cwd: s.cwd, state: s.running ? 'idle' : 'exited' })));
            setActive(list[0]?.id ?? null);
          } else return newSession();
        })
        .catch(() => newSession())
        .finally(() => (starting.current = false));
    }
  };

  const close = (id: string) => {
    void ops.close(id).catch(() => undefined);
    views.current.delete(id);
    setTabs((t) => {
      const rest = t.filter((x) => x.id !== id);
      if (active === id) setActive(rest.at(-1)?.id ?? null);
      if (rest.length === 0) setOpen(false);
      return rest;
    });
  };

  useBinding({ id: 'terminal.toggle', title: 'Toggle terminal', defaultKeys: 'Ctrl+`', run: toggle });
  useBinding({ id: 'terminal.toggleAlt', title: 'Toggle terminal (alternative)', defaultKeys: 'Ctrl+J', run: toggle });
  useLayer('search', finding, () => setQuery(''), () => query.length > 0);
  useLayer(
    'panel',
    finding || max,
    () => {
      if (finding) {
        setFinding(false);
        views.current.get(active ?? '')?.clearFind();
        views.current.get(active ?? '')?.focus();
      } else setMax(false);
    },
  );

  const command = (cmd: string, from: DockTab) => {
    if (cmd === 'new') void newSession(from);
    else if (cmd === 'close') close(from.id);
    else if (cmd === 'find') setFinding(true);
    else if (cmd.startsWith('select:')) {
      const t = tabs[Number(cmd.slice(7)) - 1];
      if (t) setActive(t.id);
    }
  };

  const startResize = (e: React.PointerEvent) => {
    if (max) return;
    const startY = e.clientY;
    const startH = height;
    const pane = dock.current?.parentElement?.getBoundingClientRect().height ?? window.innerHeight;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    let last = startH;
    const move = (ev: PointerEvent) => {
      last = clampHeight(startH + (startY - ev.clientY), pane);
      setHeight(last);
    };
    const up = () => {
      target.removeEventListener('pointermove', move);
      target.removeEventListener('pointerup', up);
      void shell.api('preference', { key: 'terminalHeight', value: last }).catch(() => undefined);
    };
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', up);
  };

  if (!loaded) return null;
  const openLink = (url: string) => void shellOps(shell.api).openExternal(url).catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) }));
  return (
    <div
      ref={dock}
      className={`term-dock${max ? ' max' : ''}`}
      data-testid="terminal-dock"
      data-open={open}
      hidden={!open}
      style={max ? undefined : { height }}
    >
      <div className="term-handle" onPointerDown={startResize} onDoubleClick={() => setMax((m) => !m)} />
      <div className="term-bar">
        <div className="term-tabs" role="tablist" aria-label="Terminals">
          {tabs.map((t, i) => (
            <div
              key={t.id}
              role="tab"
              tabIndex={0}
              aria-selected={t.id === active}
              className="term-tab"
              data-testid="terminal-tab"
              data-state={t.state}
              title={`${t.title} · ${t.cwd}${i < 9 ? ` (Ctrl+Shift+${i + 1})` : ''}`}
              onClick={() => {
                setActive(t.id);
                setTabs((all) => all.map((x) => (x.id === t.id ? { ...x, state: nextTabState(x.state, 'seen', true) } : x)));
              }}
              onAuxClick={(e) => e.button === 1 && close(t.id)}
            >
              <span className={`term-dot ${t.state}`} aria-hidden="true" />
              <span className="term-title">{t.title}</span>
              <span className="term-cwd">{t.cwd}</span>
              <button
                type="button"
                className="term-x"
                aria-label="Close terminal"
                title="Close terminal"
                onClick={(e) => {
                  e.stopPropagation();
                  close(t.id);
                }}
              >
                ×
              </button>
            </div>
          ))}
        </div>
        <div className="term-actions">
          <Button variant="icon" label="New terminal (Ctrl+Shift+T)" onClick={() => void newSession(tabs.find((t) => t.id === active))}>
            +
          </Button>
          <Button variant="icon" label="Find (Ctrl+Shift+F)" pressed={finding} onClick={() => setFinding((f) => !f)}>
            ⌕
          </Button>
          <Button variant="icon" label={max ? 'Restore' : 'Maximise'} pressed={max} onClick={() => setMax((m) => !m)}>
            ⤢
          </Button>
          <Button variant="icon" label="Hide terminal (Ctrl+`)" onClick={() => setOpen(false)}>
            ⌄
          </Button>
        </div>
      </div>
      {finding && (
        <div className="term-find">
          <input
            autoFocus
            placeholder="Find"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              views.current.get(active ?? '')?.find(e.target.value);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') views.current.get(active ?? '')?.find(query, e.shiftKey);
            }}
          />
        </div>
      )}
      <div className="term-body">
        {tabs.map((t) => (
          <XtermView
            key={t.id}
            id={t.id}
            ops={ops}
            bus={shell.bus}
            visible={open && t.id === active}
            ref={(h) => {
              if (h) views.current.set(t.id, h);
            }}
            onOutput={() =>
              setTabs((all) => {
                const x = all.find((y) => y.id === t.id);
                const next = x ? nextTabState(x.state, 'output', open && t.id === active) : 'idle';
                return x && x.state !== next ? all.map((y) => (y.id === t.id ? { ...y, state: next } : y)) : all;
              })
            }
            onExit={(code) => setTabs((all) => all.map((y) => (y.id === t.id ? { ...y, state: 'exited', exitCode: code } : y)))}
            onCommand={(cmd) => command(cmd, t)}
            openLink={openLink}
          />
        ))}
      </div>
    </div>
  );
}
