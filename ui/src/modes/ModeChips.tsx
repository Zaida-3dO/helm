// The chip row by the composer: the modes switched on in this chat, each with a cross to turn it
// off, and a "+ mode" menu with the rest. A chat that has not started yet shows its project's
// default modes as already on. The row reloads when a mode is changed by the agent (it files a
// transcript row), when a mode file or the project's defaults change, and after its own change.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { projectOps, type ProjectNode } from '../api/projects.ts';
import type { MessageRow } from '../api/types.ts';
import { useShell } from '../shell/context.ts';
import { Chip, ContextMenu } from '../ui/index.ts';
import { modeOps, type ChatModes, type ChatModesChange } from './api.ts';
import { applyChange, defaultsOf, hasModes, menuModes, shown } from './chips.ts';
import './modes.css';

const SOURCE_NOTE: Record<string, string> = {
  project: 'Switched on by the project',
  user: 'Switched on by you',
  agent: 'Switched on by the agent',
  carried: 'Carried over from the earlier chat',
};

export function ModeChips({ chatId, projectId, started }: { chatId: string; projectId: string; started: boolean }) {
  const shell = useShell();
  const ops = useMemo(() => modeOps(shell.api), [shell.api]);
  const [modes, setModes] = useState<ChatModes | null>(null);
  const [projects, setProjects] = useState<ProjectNode[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  // A read that a newer one (or a change of mine) overtook is dropped.
  const seq = useRef(0);

  const load = useCallback(() => {
    const mine = ++seq.current;
    return ops
      .chatGet(chatId)
      .then((m) => {
        if (mine !== seq.current) return;
        setModes(m);
        setError(null);
      })
      .catch((e: unknown) => {
        if (mine === seq.current) setError(String((e as Error)?.message ?? e));
      });
  }, [ops, chatId]);

  useEffect(() => {
    setModes(null);
    setMenu(null);
    const offs = [
      shell.bus.on('knowledge-changed', () => void load()),
      shell.bus.on('refresh', (e) => e.chatId === chatId && void load()),
      shell.bus.on('message', (e) => {
        if (e.chatId !== chatId) return;
        const rows = (e.rows as MessageRow[] | undefined) ?? [];
        if (rows.some((r) => r.role === 'context' && r.details?.contextKind === 'modes')) void load();
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [shell, chatId, load]);

  // Read for this chat, and again once it has started: the host has put the defaults on by then.
  useEffect(() => {
    void load();
  }, [started, load]);

  // Only a chat that has not started needs its project's defaults.
  useEffect(() => {
    if (started) return;
    let live = true;
    const read = () =>
      projectOps(shell.api)
        .list(true, true)
        .then((l) => live && setProjects(l))
        .catch(() => undefined);
    void read();
    const off = shell.bus.on('project', () => void read());
    return () => {
      live = false;
      off();
    };
  }, [shell, started, projectId]);

  const defaults = useMemo(() => defaultsOf(projects.find((p) => p.id === projectId), projects), [projects, projectId]);
  const view = useMemo(() => (modes ? shown(modes, started, defaults) : null), [modes, started, defaults]);
  if (!view || !hasModes(view)) return error ? <ModeError text={error} /> : null;

  const change = (c: ChatModesChange) => {
    const before = view;
    seq.current++;
    setModes(applyChange(view, c));
    setError(null);
    ops
      .chatSet(chatId, c)
      .then((m) => {
        setModes(m);
      })
      .catch((e: unknown) => {
        setModes(before);
        setError(String((e as Error)?.message ?? e));
      });
  };

  const rest = menuModes(view);
  return (
    <div className="mode-chips" data-testid="mode-chips">
      {view.active.map((m) => (
        <span key={m.name} className="mode-chip" data-testid={`mode-chip-${m.name}`} data-source={m.source}>
          <Chip title={`${m.description || m.name} · ${SOURCE_NOTE[m.source] ?? m.source}`} onRemove={() => change({ remove: [m.name] })}>
            {m.name}
          </Chip>
        </span>
      ))}
      {rest.length > 0 && (
        <button
          type="button"
          className="mode-add"
          aria-haspopup="menu"
          aria-expanded={menu !== null}
          data-testid="mode-add"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setMenu({ x: r.left, y: Math.max(8, r.top - (rest.length * 30 + 12)) });
          }}
        >
          + mode
        </button>
      )}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label="Add a mode"
          testId="mode-menu"
          onClose={() => setMenu(null)}
          items={rest.map((m) => ({
            id: m.name,
            label: (
              <span title={m.description} data-testid={`mode-menu-${m.name}`}>
                {m.name}
                {m.off && <span className="muted"> · off</span>}
              </span>
            ),
            onSelect: () => change({ add: [m.name] }),
          }))}
        />
      )}
      {error && <ModeError text={error} />}
    </div>
  );
}

function ModeError({ text }: { text: string }) {
  return (
    <span className="mode-error" role="alert" data-testid="mode-error">
      {text}
    </span>
  );
}
