// The top bar's "…": the same chat menu as a sidebar row's, built from
// the same item list (menus.ts). It reads the row and the projects when opened, so it never goes
// stale and never waits on the board.

import { useMemo, useState } from 'react';
import { projectOps, type ProjectNode, type SidebarRow } from '../api/projects.ts';
import { useShell } from '../shell/context.ts';
import { ConfirmDialog, ContextMenu, toast, type ConfirmSpec, type MenuItem } from '../ui/index.ts';
import { SidebarActions, compactChat, exportChatMarkdown, rollOverChat, rollOverConfirm } from './actions.ts';
import { SidebarData } from './data.ts';
import { chatMenuItems } from './menus.ts';
import { withChatIcons } from './menuIcons.tsx';
import { IconMore } from '../ui/icons.tsx';
import { deleteChatConfirm, PromptDialog, type Prompt } from './Sidebar.tsx';
import { sidebarUndo } from './undo.ts';
import { CostDialog } from '../usage/CostDialog.tsx';
import { getDefaultBoard } from '../newchat/boards.ts';
import { moveChatToProfile, profileChoices, profilesStoreFor, shareChatWith, shareChoices } from '../profiles/index.ts';
import type { DeviceInfo, RemoteChatRecord } from '../devices/api.ts';
import { devicesStoreFor } from '../devices/store.ts';
import { remoteProfileChoices, remoteProject, remoteRow, remoteShareChoices } from '../devices/model.ts';

// Errors go to the shell's toast slot, where the user sees them.
const showError = (text: string) => toast.show({ kind: 'error', text });

export function ChatMenu({ chatId }: { chatId: string | null }) {
  const shell = useShell();
  const pops = useMemo(() => projectOps(shell.api), [shell.api]);
  const data = useMemo(() => new SidebarData(pops), [pops]);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [cost, setCost] = useState<string | null>(null);
  const act = useMemo(
    () => new SidebarActions({ api: shell.api, pops, ops: shell.ops, data, undo: sidebarUndo, onError: showError }),
    [shell.api, shell.ops, pops, data],
  );

  const open = async (x: number, y: number) => {
    if (!chatId) return;
    try {
      const [rows, projects, boards] = await Promise.all([pops.rows([chatId]), pops.list(false, true), getDefaultBoard(shell.api)]);
      let row: SidebarRow | undefined = rows[0];
      let siblings: SidebarRow[] = [];
      // A chat that runs on another device: the mirror answers `chat_get` for it, and its
      // group and profiles are that device's. The menu is the same; the ops route over the wire.
      let copy: RemoteChatRecord | undefined;
      let device: DeviceInfo | null = null;
      let shownProjects = projects as ProjectNode[];
      let hidden = boards.hiddenBoardIds;
      if (!row) {
        copy = (await shell.api<{ chat: RemoteChatRecord }>('chat_get', { chatId })).chat;
        const hostUid = copy.hostDeviceId;
        device = devicesStoreFor(shell.api, shell.bus).snapshot.list?.devices.find((d) => d.uid === hostUid) ?? null;
        if (!device) return;
        row = remoteRow(copy, false);
        shownProjects = (await devicesStoreFor(shell.api, shell.bus).ops.projects(device.uid)).map((p) => remoteProject(p, 0));
      } else {
        siblings = (await pops.page(row.archived ? 'archived' : 'projects', undefined, row.groupProjectId)).rows;
      }
      const r = row;
      const items = withChatIcons(chatMenuItems(
        {
          row: r,
          projects: shownProjects,
          hidden,
          siblings,
          heightOf: () => 0,
          depthOf: (id) => siblings.find((s) => s.chatId === id)?.depth ?? 0,
          expanded: false,
          view: r.archived ? 'archived' : 'projects',
          profiles: copy && device ? remoteProfileChoices(device, copy) : profileChoices(profilesStoreFor(shell.api, shell.bus).snapshot, r.chatId, siblings),
          shares: copy && device ? remoteShareChoices(device, copy) : shareChoices(profilesStoreFor(shell.api, shell.bus).snapshot, r.chatId, [r, ...siblings]),
        },
        {
          fork: () => void act.fork(r),
          rename: () => setPrompt({ title: 'Rename chat', label: 'Title', value: r.title, suggestFor: r.chatId, onSave: (t) => act.renameChat(r, t) }),
          compact: () => void compactChat(shell.ops, r.chatId, showError),
          rollOver: () => setConfirm(rollOverConfirm(r.title, () => rollOverChat(shell.ops, r.chatId, showError))),
          cost: () => setCost(r.chatId),
          exportMarkdown: () => void exportChatMarkdown(shell.api, r.chatId, showError),
          moveTo: (id) => void act.moveTo(r, id),
          moveToProfile: (id) => {
            if (copy && device) {
              void devicesStoreFor(shell.api, shell.bus).ops.moveProfile({ chatId: r.chatId, profileId: id }).catch((e) => showError(String((e as Error)?.message ?? e)));
              return;
            }
            moveChatToProfile(profilesStoreFor(shell.api, shell.bus), r.chatId, id, showError);
          },
          shareWith: (id) => {
            if (copy && device) {
              const shared = !(copy.sharedWithPeople ?? []).includes(id);
              void devicesStoreFor(shell.api, shell.bus).ops.shareChat({ chatId: r.chatId, personId: id, shared }).catch((e) => showError(String((e as Error)?.message ?? e)));
              return;
            }
            const store = profilesStoreFor(shell.api, shell.bus);
            shareChatWith(store, r.chatId, shareChoices(store.snapshot, r.chatId, [r, ...siblings]), id, showError);
          },
          nestUnder: (id) => void act.nestUnder(r, id),
          topLevel: () => void act.topLevel(r),
          toggleChildren: () => undefined,
          markUnread: () => void act.markUnread(r),
          pin: (p) => void act.pin(r, p),
          mute: (m) => void act.mute(r, m),
          archive: () => void act.archive(r),
          restore: () => void act.restore(r),
          remove: () => setConfirm(deleteChatConfirm(r, () => act.remove(r), device?.name)),
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
      {cost && <CostDialog api={shell.api} target={{ chatId: cost }} onClose={() => setCost(null)} />}
    </>
  );
}
