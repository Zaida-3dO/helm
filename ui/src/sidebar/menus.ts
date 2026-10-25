// The chat and project menus as pure item lists,
// so the row "…", right-click, the top bar's "…" and tests all build the same menu. Items for ops
// the host has not shipped yet still appear; a refusal shows in the sidebar's error line.

import type { MenuItem } from '../ui/index.ts';
import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { MAX_DEPTH } from './model.ts';
import { primaryFolderMenuItem } from '../newchat/menu.ts';

export const PROJECT_COLOURS = ['red', 'orange', 'yellow', 'green', 'teal', 'blue', 'purple', 'pink'] as const;

export interface ChatMenuActions {
  fork(): void;
  rename(): void;
  /** Compact the chat's context (the `chat_compact` op). */
  compact(): void;
  /** Hand the chat's work to a fresh chat (the `chat_roll_over` op), after a confirmation. */
  rollOver(): void;
  /** Show what the chat has used: tokens, estimated cost, weekly share. */
  cost(): void;
  moveTo(projectId: string): void;
  nestUnder(chatId: string): void;
  topLevel(): void;
  toggleChildren(): void;
  markUnread(): void;
  pin(pinned: boolean): void;
  mute(muted: boolean): void;
  archive(): void;
  restore(): void;
  remove(): void;
}

export interface ChatMenuInput {
  row: SidebarRow;
  /** Every project, flat (for "Move to project"). */
  projects: ProjectNode[];
  /** Rows held for the same group, for "Nest under…". */
  siblings: SidebarRow[];
  /** Depth of each held row's subtree below it (0 = no children). */
  heightOf: (chatId: string) => number;
  depthOf: (chatId: string) => number;
  expanded: boolean;
  view: 'projects' | 'activity' | 'archived';
}

/** Valid "Nest under…" targets: same group and archived state, not itself or its descendants, and
 *  the moved subtree must stay within depth index 2. */
export function nestTargets(i: ChatMenuInput): SidebarRow[] {
  const descendants = new Set<string>();
  const kids = new Map<string, string[]>();
  for (const r of i.siblings) {
    if (!r.parentChatId) continue;
    kids.set(r.parentChatId, [...(kids.get(r.parentChatId) ?? []), r.chatId]);
  }
  const walk = (id: string) => {
    for (const k of kids.get(id) ?? []) {
      if (!descendants.has(k)) {
        descendants.add(k);
        walk(k);
      }
    }
  };
  walk(i.row.chatId);
  const height = i.heightOf(i.row.chatId);
  return i.siblings.filter(
    (r) =>
      r.chatId !== i.row.chatId &&
      r.chatId !== i.row.parentChatId &&
      !descendants.has(r.chatId) &&
      r.archived === i.row.archived &&
      r.groupProjectId === i.row.groupProjectId &&
      i.depthOf(r.chatId) + 1 + height <= MAX_DEPTH,
  );
}

export function chatMenuItems(i: ChatMenuInput, a: ChatMenuActions): MenuItem[] {
  const r = i.row;
  const busy = r.status === 'starting' || r.status === 'running' || r.status === 'stopping';
  const archivedGroup = i.projects.find((p) => p.id === r.groupProjectId)?.archived ?? false;
  const items: MenuItem[] = [
    { id: 'fork', label: 'Fork chat', shortcut: 'Ctrl+Shift+F', disabled: archivedGroup, onSelect: a.fork },
    { id: 'rename', label: 'Rename chat…', shortcut: 'Ctrl+Alt+R', onSelect: a.rename },
    // Compacting summarises the conversation so far to free context; not while a turn runs.
    { id: 'compact', label: 'Compact context', disabled: r.archived || busy, onSelect: a.compact },
    // Rolling over needs the chat idle: the agent writes the hand-off in a turn of its own.
    { id: 'rollover', label: 'Roll over…', disabled: r.archived || busy || archivedGroup, onSelect: a.rollOver },
    { id: 'cost', label: 'Cost…', onSelect: a.cost },
    {
      id: 'move',
      label: 'Move to project',
      disabled: r.archived || archivedGroup,
      submenu: i.projects
        .filter((p) => !p.archived)
        .map((p) => ({
          id: `move:${p.id}`,
          label: p.name,
          checked: p.id === r.groupProjectId,
          disabled: p.id === r.groupProjectId,
          onSelect: () => a.moveTo(p.id),
        })),
    },
  ];
  if (i.view !== 'activity') {
    const targets = nestTargets(i);
    items.push({
      id: 'nest',
      label: 'Nest under…',
      disabled: targets.length === 0,
      submenu: targets.map((t) => ({ id: `nest:${t.chatId}`, label: t.title, onSelect: () => a.nestUnder(t.chatId) })),
    });
    if (r.parentChatId) items.push({ id: 'top', label: 'Move to top level', onSelect: a.topLevel });
    if (r.hasChildren) {
      items.push({ id: 'children', label: i.expanded ? 'Collapse started chats' : 'Expand started chats', onSelect: a.toggleChildren });
    }
  }
  items.push(
    { id: 'unread', label: 'Mark as unread', shortcut: 'Ctrl+Shift+U', disabled: r.unread, onSelect: a.markUnread },
    { id: 'pin', label: r.pinned ? 'Unpin chat' : 'Pin chat', shortcut: 'Ctrl+Alt+P', onSelect: () => a.pin(!r.pinned) },
    { id: 'mute', label: r.muted ? 'Unmute chat' : 'Mute chat', onSelect: () => a.mute(!r.muted) },
  );
  if (r.archived) {
    items.push({ id: 'restore', label: 'Restore chat', onSelect: a.restore });
    items.push({ id: 'delete', label: 'Delete permanently…', danger: true, disabled: busy, onSelect: a.remove });
  } else {
    items.push({
      id: 'archive',
      label: 'Archive chat',
      shortcut: 'Ctrl+Shift+Backspace',
      onSelect: a.archive,
    });
  }
  return items;
}

export interface ProjectMenuActions {
  newChat(): void;
  pin(pinned: boolean): void;
  rename(): void;
  changeIcon(): void;
  /** Show what every chat inside has used. */
  cost(): void;
  nestUnder(projectId: string): void;
  topLevel(): void;
  archive(): void;
  restore(): void;
  colour(c: string | null): void;
  customColour(): void;
}

/** Valid "Nest under…" targets for a project: not itself or its subtree, depth index ≤ 2 with its
 *  own subtree counted. */
export function projectNestTargets(p: ProjectNode, all: ProjectNode[]): ProjectNode[] {
  const byId = new Map(all.map((x) => [x.id, x]));
  const depth = (x: ProjectNode): number => {
    let d = 0;
    let cur = x.parentId ? byId.get(x.parentId) : undefined;
    const seen = new Set([x.id]);
    while (cur && !seen.has(cur.id)) {
      seen.add(cur.id);
      d++;
      cur = cur.parentId ? byId.get(cur.parentId) : undefined;
    }
    return d;
  };
  const sub = new Set<string>([p.id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const x of all) if (x.parentId && sub.has(x.parentId) && !sub.has(x.id)) (sub.add(x.id), (grew = true));
  }
  const height = (id: string): number => {
    const kids = all.filter((x) => x.parentId === id && x.id !== id);
    return kids.length === 0 ? 0 : 1 + Math.max(...kids.map((k) => height(k.id)));
  };
  const h = height(p.id);
  return all.filter((x) => !sub.has(x.id) && !x.archived && x.id !== p.parentId && depth(x) + 1 + h <= MAX_DEPTH);
}

export function projectMenuItems(p: ProjectNode, all: ProjectNode[], a: ProjectMenuActions): MenuItem[] {
  const targets = projectNestTargets(p, all);
  const items: MenuItem[] = [
    { id: 'new', label: 'New chat', disabled: p.archived, onSelect: a.newChat },
    { id: 'pin', label: p.pinned ? 'Unpin project' : 'Pin project', disabled: p.archived, onSelect: () => a.pin(!p.pinned) },
    { id: 'rename', label: 'Rename project…', onSelect: a.rename },
    primaryFolderMenuItem(p),
    { id: 'icon', label: 'Change icon…', onSelect: a.changeIcon },
    { id: 'cost', label: 'Cost…', onSelect: a.cost },
    {
      id: 'nest',
      label: 'Nest under…',
      disabled: targets.length === 0,
      submenu: targets.map((t) => ({ id: `nest:${t.id}`, label: t.name, onSelect: () => a.nestUnder(t.id) })),
    },
  ];
  if (p.parentId) items.push({ id: 'top', label: 'Move to top level', onSelect: a.topLevel });
  items.push(
    p.archived
      ? { id: 'restore', label: 'Restore project', onSelect: a.restore }
      : { id: 'archive', label: 'Archive project…', onSelect: a.archive },
    {
      id: 'colour',
      label: 'Colour',
      separator: true,
      submenu: [
        { id: 'colour:none', label: 'No colour', checked: p.colour === null, onSelect: () => a.colour(null) },
        ...PROJECT_COLOURS.map((c) => ({
          id: `colour:${c}`,
          label: c[0]!.toUpperCase() + c.slice(1),
          checked: p.colour === c,
          onSelect: () => a.colour(c),
        })),
        { id: 'colour:custom', label: 'Custom…', separator: true, checked: p.colour?.startsWith('#') === true, onSelect: a.customColour },
      ],
    },
  );
  return items;
}
