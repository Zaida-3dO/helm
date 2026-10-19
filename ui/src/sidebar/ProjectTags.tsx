// The Project / Workspace tags under the top bar.
// Project: the group the open chat is shown under; picking another writes `chat_groups` through
// `chat_organize` (the first option, named for the folder project, puts it back there). Workspace: the folder it runs in,
// which grouping never changes. Both read the cached project list; nothing waits on the board.

import { useEffect, useMemo, useState } from 'react';
import { projectOps, type ProjectNode, type SidebarRow } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { Picker, toast, type PickerOption } from '../ui/index.ts';

const NONE = '__none__';

export function ProjectTags({ chatId }: { chatId: string | null }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);
  const [row, setRow] = useState<SidebarRow | null>(null);
  const [projects, setProjects] = useState<ProjectNode[]>([]);

  useEffect(() => {
    let live = true;
    const load = () => {
      if (!chatId) {
        setRow(null);
        return;
      }
      Promise.all([pops.rows([chatId]), pops.list(false)])
        .then(([rows, list]) => {
          if (!live) return;
          setRow(rows[0] ?? null);
          setProjects(list);
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
  }, [chatId, pops, shell.bus]);

  if (!chatId || !row) return null;
  const group = projects.find((p) => p.id === row.groupProjectId);
  // A chat shown under its own folder project is labelled with that project's name, not
  // "None"; picking another project moves it into that group.
  const folderProject = projects.find((p) => p.id === row.folderProjectId);
  const folderName = folderProject?.name ?? baseName(row.folderPath);
  const options: PickerOption<string>[] = [
    { value: NONE, title: folderName, text: folderName, subtitle: 'Its own folder project' },
    ...projects
      .filter((p) => p.kind === 'board' || p.id !== row.folderProjectId)
      .map((p) => ({ value: p.id, title: p.name, text: p.name, subtitle: p.kind === 'board' ? 'Agent Standup' : (p.path ?? 'Local group') })),
  ];
  const value = row.groupProjectId === row.folderProjectId ? NONE : row.groupProjectId;
  const shown = value === NONE ? folderName : (group?.name ?? folderName);
  const choose = (v: string) => {
    const groupProjectId = v === NONE ? row.folderProjectId : v;
    if (groupProjectId === row.groupProjectId) return;
    pops
      .organize({ chatId: row.chatId, groupProjectId, parentChatId: null })
      .then((rows) => setRow(rows.find((r) => r.chatId === row.chatId) ?? row))
      .catch((e: unknown) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }));
  };
  return (
    <div className="sb-tags" data-testid="project-tags">
      <Picker
        value={value}
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
        <span className="sb-tag-label">Workspace:</span>
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
