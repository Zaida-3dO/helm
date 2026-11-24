// The chat's project pill in the top bar, drawn after the title. It names the board the chat is
// shown under; the popover also carries the chat's folder moves: picking a board re-shelves the
// chat (chat_organize), "Change folder…" points it at another folder on this PC (the native folder
// dialog, then chat_set_folder — a busy chat is refused, and moving back is the same two steps),
// and "New project…" opens the New project form. Only boards are offered as projects; a folder is
// where a chat runs, not where it is shown.

import { useEffect, useMemo, useState } from 'react';
import { projectOps, type ProjectNode, type SidebarRow } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { Picker, toast, type PickerOption } from '../ui/index.ts';
import { getDefaultBoard } from '../newchat/boards.ts';
import { pickerProjects } from '../newchat/places.ts';
import { openNewProject } from '../newchat/index.ts';
import { syncOnOpen } from '../sidebar/sync.ts';
import { baseName } from '../sidebar/ProjectTags.tsx';

/** The popover's action row after the board list, which no search filters out. */
const NEW_PROJECT = '__new-project__';

export function ProjectPill({ chatId }: { chatId: string | null }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);
  // Opening the picker syncs the boards quietly (at most every 30 seconds); the list updates when it reports in.
  const syncBoards = useMemo(() => syncOnOpen(() => void pops.sync().catch(() => undefined)), [pops]);
  const [row, setRow] = useState<SidebarRow | null>(null);
  const [projects, setProjects] = useState<ProjectNode[]>([]);
  const [hidden, setHidden] = useState<string[]>([]);

  useEffect(() => {
    let live = true;
    const load = () => {
      if (!chatId) {
        setRow(null);
        return;
      }
      Promise.all([pops.rows([chatId]), pops.list(false, true), getDefaultBoard(shell.api)])
        .then(([rows, list, boards]) => {
          if (!live) return;
          setRow(rows[0] ?? null);
          setProjects(list);
          setHidden(boards.hiddenBoardIds);
        })
        .catch(() => undefined);
    };
    load();
    const offs = [
      shell.bus.on('sidebar', (e) => {
        if (chatId && ((e.chatIds as string[]) ?? []).includes(chatId)) load();
      }),
      shell.bus.on('project', load),
      shell.bus.on('board_sync', load),
    ];
    return () => {
      live = false;
      offs.forEach((o) => o());
    };
  }, [chatId, pops, shell.api, shell.bus]);

  if (!chatId || !row) return null;
  const group = projects.find((p) => p.id === row.groupProjectId);
  // A chat not on a board yet (still under its folder) shows that folder's name until it is moved.
  const shown = group ? group.name : baseName(row.folderPath);
  const options: PickerOption<string>[] = pickerProjects(projects, false, row.groupProjectId, hidden).map((p) => ({
    value: p.id,
    title: p.name,
    text: p.name,
    subtitle: p.kind === 'board' ? 'Agent Standup' : 'A folder, not a board',
  }));
  const err = (e: unknown) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) });
  const choose = (id: string, query: string) => {
    if (id === NEW_PROJECT) {
      // The New project form, with whatever was typed in the search as its name.
      openNewProject(query.trim() ? { title: query.trim() } : {});
      return;
    }
    if (id === row.groupProjectId) return;
    pops
      .organize({ chatId: row.chatId, groupProjectId: id, parentChatId: null })
      .then((rows) => setRow(rows.find((r) => r.chatId === row.chatId) ?? row))
      .catch(err);
  };
  // Move the chat's folder: the native dialog picks it, then the host repoints the chat at it.
  // A working chat is refused by the host ("stop it first"), which the toast repeats.
  const changeFolder = async () => {
    const picked = await shell.ops.pickFolder().catch((e: unknown) => {
      err(e);
      return null;
    });
    if (!picked?.path) return;
    try {
      await shell.api('chat_set_folder', { chatId: row.chatId, cwd: picked.path });
      toast.show({ kind: 'info', text: `This chat now runs in ${baseName(picked.path)}.` });
    } catch (e) {
      err(e);
    }
  };
  return (
    <Picker
      className="project-pill"
      value={row.groupProjectId}
      options={options}
      onChange={choose}
      label={`Project: ${shown}`}
      title={`${shown} · ${row.folderPath}`}
      display={<span className="project-pill-name">{shown}</span>}
      heading="Show this chat under"
      searchable
      onOpen={syncBoards}
      testId="project-pill"
      extraOption={(query) => ({
        value: NEW_PROJECT,
        title: query ? `New project “${query.trim()}”…` : 'New project…',
        text: 'new project',
      })}
      footer={
        <div className="project-pill-footer">
          <button type="button" data-testid="project-pill-folder" onClick={() => void changeFolder()}>
            Change folder… ({baseName(row.folderPath)})
          </button>
        </div>
      }
    />
  );
}
