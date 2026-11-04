// The Project / Folder tags under the top bar.
// Project: the board the open chat is shown under; picking another board writes `chat_groups`
// through `chat_organize`. Only boards are offered: a folder is where a chat runs, not where it is
// shown. Folder: the folder it runs in, which moving it between boards never changes. Both read the
// cached project list; nothing waits on the board.

import { useEffect, useMemo, useState } from 'react';
import { projectOps, type ProjectNode, type SidebarRow } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { Picker, toast, type PickerOption } from '../ui/index.ts';
import { getDefaultBoard } from '../newchat/boards.ts';
import { pickerProjects } from '../newchat/places.ts';

export function ProjectTags({ chatId }: { chatId: string | null }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);
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
      Promise.all([pops.rows([chatId]), pops.list(false), getDefaultBoard(shell.api)])
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
  const shown = group?.name ?? baseName(row.folderPath);
  const options: PickerOption<string>[] = pickerProjects(projects, false, row.groupProjectId, hidden).map((p) => ({
    value: p.id,
    title: p.name,
    text: p.name,
    subtitle: p.kind === 'board' ? 'Agent Standup' : 'A folder, not a board',
  }));
  const choose = (groupProjectId: string) => {
    if (groupProjectId === row.groupProjectId) return;
    pops
      .organize({ chatId: row.chatId, groupProjectId, parentChatId: null })
      .then((rows) => setRow(rows.find((r) => r.chatId === row.chatId) ?? row))
      .catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }));
  };
  return (
    <div className="sb-tags" data-testid="project-tags">
      <Picker
        value={row.groupProjectId}
        options={options}
        onChange={choose}
        label={`Project: ${shown}`}
        title="Change this chat's project"
        display={
          <>
            <span className="sb-tag-label">Project:</span>
            <span className="sb-tag-value">{shown}</span>
          </>
        }
        heading="Show this chat under"
        searchable
        testId="project-tag"
      />
      <span className="sb-tag" data-testid="workspace-tag" title={row.folderPath}>
        <span className="sb-tag-label">Folder:</span>
        <span className="sb-tag-value">{baseName(row.folderPath)}</span>
      </span>
    </div>
  );
}

/** The last segment of a folder path ("C:\work\app" → "app"). */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}
