// The top bar's "…": the same chat menu as a sidebar row's, built from
// the same item list (menus.ts). It reads the row and the projects when opened, so it never goes
// stale and never waits on the board.

import { useMemo, useState } from 'react';
import { projectOps, type ProjectNode, type SidebarRow } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { ConfirmDialog, ContextMenu, toast, type ConfirmSpec, type MenuItem } from '../ui/index.ts';
import { SidebarActions, compactChat, rollOverChat, rollOverConfirm } from './actions.ts';
import { SidebarData } from './data.ts';
import { chatMenuItems } from './menus.ts';
import { withChatIcons } from './menuIcons.tsx';
import { IconMore } from '../ui/icons.tsx';
import { deleteChatConfirm, PromptDialog, type Prompt } from './Sidebar.tsx';
import { StopArchiveDialog } from './StopArchiveDialog.tsx';
import { isBusy } from './status.ts';
import { sidebarUndo } from './undo.ts';

// Errors go to the shell's toast slot, where the user sees them.
const showError = (text: string) => toast.show({ kind: 'error', text });

export function ChatMenu({ chatId }: { chatId: string | null }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);
  const data = useMemo(() => new SidebarData(pops), [pops]);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [stopRow, setStopRow] = useState<SidebarRow | null>(null);
  const act = useMemo(
    () => new SidebarActions({ api: shell.api, pops, ops: shell.ops, data, undo: sidebarUndo, onError: showError }),
    [shell.api, shell.ops, pops, data],
  );

  const open = async (x: number, y: number) => {
    if (!chatId) return;
    try {
      const [rows, projects] = await Promise.all([pops.rows([chatId]), pops.list(false)]);
      const row: SidebarRow | undefined = rows[0];
      if (!row) return;
      const siblings = (await pops.page(row.archived ? 'archived' : 'projects', undefined, row.groupProjectId)).rows;
      const items = withChatIcons(chatMenuItems(
        {
          row,
          projects: projects as ProjectNode[],
          siblings,
          heightOf: () => 0,
          depthOf: (id) => siblings.find((s) => s.chatId === id)?.depth ?? 0,
          expanded: false,
          view: row.archived ? 'archived' : 'projects',
        },
        {
          fork: () => void act.fork(row),
          rename: () => setPrompt({ title: 'Rename chat', label: 'Title', value: row.title, onSave: (t) => act.renameChat(row, t) }),
          compact: () => void compactChat(shell.ops, row.chatId, showError),
          rollOver: () => setConfirm(rollOverConfirm(row.title, () => rollOverChat(shell.ops, row.chatId, showError))),
          moveTo: (id) => void act.moveTo(row, id),
          nestUnder: (id) => void act.nestUnder(row, id),
          topLevel: () => void act.topLevel(row),
          toggleChildren: () => undefined,
          markUnread: () => void act.markUnread(row),
          pin: (p) => void act.pin(row, p),
          mute: (m) => void act.mute(row, m),
          archive: () => (isBusy(row.status) ? setStopRow(row) : void act.archive(row)),
          restore: () => void act.restore(row),
          remove: () => setConfirm(deleteChatConfirm(row, () => act.remove(row))),
        },
      ).filter((i) => i.id !== 'children'));
      setMenu({ x, y, items });
    } catch (e) {
      showError(String((e as Error)?.message ?? e));
    }
  };

  return (
    <>
      <button
        type="button"
        className="ui-btn ui-btn-icon"
        data-testid="chat-menu"
        aria-label="Chat options"
        title="Chat options"
        disabled={!chatId}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          void open(r.right, r.bottom);
        }}
      >
        <IconMore size={16} />
      </button>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} label="Chat options" onClose={() => setMenu(null)} testId="chat-menu-list" />}
      {prompt && <PromptDialog prompt={prompt} onClose={() => setPrompt(null)} />}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
      {stopRow && <StopArchiveDialog title={stopRow.title} onClose={() => setStopRow(null)} onConfirm={() => void act.stopAndArchive(stopRow)} />}
    </>
  );
}
