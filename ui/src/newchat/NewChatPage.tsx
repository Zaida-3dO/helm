// The new-chat page: heading, one card with the
// project picker and a place line (folder ▾ and a short hint), and the ordinary composer docked
// below. `NewChatHost` is mounted once in the app frame's chat area; it draws the page only while
// it is open, and registers Ctrl+N.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { projectOps, type ProjectNode } from '../api/projects.ts';
import { Composer } from '../composer/Composer.tsx';
import type { SendShortcut } from '../composer/keymap.ts';
import type { Flushers } from '../shell/quit.ts';
import { useShell } from '../shell/context.ts';
import { ContextMenu, Picker, toast, useBinding, type MenuItem, type PickerOption } from '../ui/index.ts';
import { pickFolder } from './api.ts';
import { draftOps, newChat } from './controller.ts';
import { folderMenu, folderName, samePath } from './places.ts';
import './newchat.css';

const NO_PROJECT = '__none__';

/** Mounted once in the frame's chat area (App.tsx). */
export function NewChatHost({ flushers, sendShortcut }: { flushers: Flushers; sendShortcut: SendShortcut }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);

  // Connect the controller to the shell: selection, the chat store, the real ops.
  useEffect(
    () =>
      newChat.attach({
        api: shell.api,
        ops: shell.ops,
        selected: () => shell.selection.chatId,
        select: (id) => shell.select(id),
        onSelection: (f) => shell.subscribe(f),
        chatPlace: (id) => {
          const c = shell.store.view(id).chat;
          return c ? { cwd: c.cwd, projectId: c.projectId } : null;
        },
        storeChat: (c) => shell.store.chat(c),
        storage: localStorage,
        now: () => Date.now(),
        uid: () => globalThis.crypto.randomUUID(),
      }),
    [shell],
  );

  // The project list, kept current.
  useEffect(() => {
    let live = true;
    const load = () =>
      void pops
        .list(false)
        .then((l) => live && newChat.setProjects(l))
        .catch(() => undefined);
    load();
    const offs = [shell.bus.on('project', load), shell.bus.on('board_sync', load)];
    return () => {
      live = false;
      offs.forEach((o) => o());
    };
  }, [pops, shell.bus]);

  useBinding({ id: 'chat.new', title: 'New chat', defaultKeys: 'Ctrl+N', run: () => newChat.show() });

  useSyncExternalStore(newChat.subscribe, newChat.getVersion);
  if (!newChat.open || !newChat.draft) return null;
  return <NewChatPage flushers={flushers} sendShortcut={sendShortcut} />;
}

function NewChatPage({ flushers, sendShortcut }: { flushers: Flushers; sendShortcut: SendShortcut }) {
  const shell = useShell();
  const ops = useMemo(() => draftOps(shell.ops, newChat), [shell.ops]);
  const draft = newChat.draft!;
  const chat = newChat.record()!;
  const projects = newChat.projects;
  const project = projects.find((p) => p.id === draft.projectId);
  const { folder, source } = newChat.place();
  const folderBtn = useRef<HTMLButtonElement>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);

  useBinding({
    id: 'newchat.projectPicker',
    title: 'Choose the new chat’s project',
    defaultKeys: 'Ctrl+Alt+Shift+O',
    run: () => document.querySelector<HTMLButtonElement>('[data-testid="newchat-project"]')?.click(),
  });

  const options: PickerOption<string>[] = useMemo(
    () => [
      ...projects.map((p) => ({ value: p.id, title: p.name, text: `${p.name} ${p.path ?? ''}`, subtitle: subtitleOf(p) })),
      { value: NO_PROJECT, title: 'No project', subtitle: 'Use the folder’s own project' },
    ],
    [projects],
  );

  const browse = async () => {
    try {
      const path = await pickFolder(shell.api, { title: 'Choose a folder for the new chat', startIn: folder ?? undefined });
      if (path) newChat.setFolder(path);
    } catch (e) {
      toast.show({ kind: 'error', text: `Could not open the folder dialog: ${String((e as Error)?.message ?? e)}` });
    }
  };

  const menuItems = (): MenuItem[] => {
    const m = folderMenu(projects, project, newChat.recents());
    const items: MenuItem[] = m.open.map((o, i) => ({
      id: `open:${o.path}`,
      label: <FolderLabel name={o.name} path={o.path} />,
      checked: !!folder && samePath(o.path, folder),
      separator: i === 0 ? false : undefined,
      onSelect: () => newChat.setFolder(o.path),
    }));
    m.recent.forEach((r, i) =>
      items.push({
        id: `recent:${r}`,
        label: <FolderLabel name={folderName(r)} path={r} />,
        checked: !!folder && samePath(r, folder),
        separator: i === 0 && items.length > 0,
        onSelect: () => newChat.setFolder(r),
      }),
    );
    items.push({ id: 'browse', label: 'Choose another folder…', separator: items.length > 0, onSelect: () => void browse() });
    return items;
  };

  const openMenu = () => {
    const r = folderBtn.current?.getBoundingClientRect();
    if (r) setMenu({ x: r.left, y: r.bottom + 4 });
  };

  return (
    <div className="nc-page" data-testid="newchat-page">
      <div className="nc-center">
        <div className="nc-perch" aria-hidden="true" />
        <h1 className="nc-title">What are we building?</h1>
        <div className="nc-card" data-testid="newchat-card">
          <div className="nc-project">
            <Picker
              value={project ? project.id : null}
              options={options}
              onChange={(v) => newChat.move(v === NO_PROJECT ? null : v)}
              label={project ? `Project: ${project.name}` : 'Choose a project'}
              heading="Start the chat in"
              searchable
              placeholder="Choose a project"
              testId="newchat-project"
            />
          </div>
          <div className="nc-place">
            <button
              ref={folderBtn}
              type="button"
              className="nc-folder"
              data-testid="newchat-folder"
              aria-haspopup="menu"
              aria-expanded={!!menu}
              title={folder ?? 'Choose a folder for the chat to work in'}
              onClick={openMenu}
            >
              <span className="nc-folder-path">{folder ?? 'Choose a folder'}</span>
              <span className="nc-chevron" aria-hidden="true">
                ▾
              </span>
            </button>
            {hintFor(source, folder) && <span className="nc-hint">· {hintFor(source, folder)}</span>}
          </div>
        </div>
      </div>
      <div className="composer-dock nc-dock" role="region" aria-label="Composer">
        <Composer
          chat={chat}
          thread={null}
          pending={[]}
          ops={ops}
          flushers={flushers}
          sendShortcut={sendShortcut}
          onMove={() => undefined}
          registerSetText={() => undefined}
        />
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menuItems()} onClose={() => setMenu(null)} label="Folder" testId="newchat-folder-menu" />}
    </div>
  );
}

function FolderLabel({ name, path }: { name: string; path: string }) {
  return (
    <span className="nc-menu-folder">
      <span className="nc-menu-name">{name}</span>
      <span className="nc-menu-path">{path}</span>
    </span>
  );
}

function subtitleOf(p: ProjectNode): string {
  if (p.kind === 'board') return 'Agent Standup';
  return p.path ?? 'Local group';
}

/** The place line's short hint: only when the folder needs explaining. */
function hintFor(source: ReturnType<typeof newChat.place>['source'], folder: string | null): string | null {
  if (!folder) return 'no folder yet';
  if (source === 'open-chat') return 'the open chat’s folder';
  if (source === 'primary') return 'primary folder';
  return null;
}
