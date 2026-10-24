// The sidebar's list model, as pure functions.
//
// Projects view: one grouped tree. Top-level projects in host order (pinned first), a pin break
// between pinned and unpinned top-level groups, and under each expanded group first its child
// projects (one level deeper each, at most depth index 2), then its chats. Chats nest under chats
// only when the parent is held, in the same group and the same archived state; anything else is
// drawn at the top level, cycles are cut, and deeper records render at depth index 2.
// Activity view: every held chat, newest activity first, as two-line rows; no headers or nesting.
// Archived view: the projects grouping, holding only archived chats, showing only groups that hold
// one (or are themselves archived).
// Finished board projects (all their work done on the board) hide in the projects view behind
// their own "Show N finished projects" row, unless something in them still wants the user.

import type { ProjectNode, SidebarRow, SidebarView } from '../api/projects.ts';
import { arrange, type ItemRef } from '../itemlinks/folders.ts';
import { isBusy } from './status.ts';

export const MAX_DEPTH = 2;

/** Virtual heights including the 2 px gap. */
export const HEIGHTS = {
  normal: { chat: 36, twoLine: 56, header: 40, headerSubtitle: 56, pinBreak: 13 },
  compact: { chat: 28, twoLine: 38, header: 34, headerSubtitle: 50, pinBreak: 13 },
} as const;

export type ListItem =
  | { kind: 'header'; key: string; project: ProjectNode; depth: number; height: number; collapsed: boolean; subtitle: string | null }
  | {
      kind: 'chat';
      key: string;
      row: SidebarRow;
      depth: number;
      height: number;
      /** Held children under this row. */
      children: number;
      expanded: boolean;
      twoLine: boolean;
      /** The task-board item this chat is linked to, shown as a small chip. */
      itemChip?: ItemRef;
    }
  | { kind: 'itemFolder'; key: string; itemId: string; title: string; members: number; depth: number; height: number; collapsed: boolean }
  | { kind: 'pinBreak'; key: string; height: number }
  /** "Show N empty projects" / "Hide N empty projects" at the end of the projects view. */
  | { kind: 'emptyToggle'; key: string; height: number; count: number; shown: boolean }
  /** "Show N finished projects" / "Hide N finished projects", after the empty-projects row. */
  | { kind: 'finishedToggle'; key: string; height: number; count: number; shown: boolean };

export interface BuildOptions {
  view: SidebarView;
  compact: boolean;
  /** Parents the user expanded (parents are collapsed by default). */
  expandedChats: ReadonlySet<string>;
  /** The open chat: its group and ancestors stay visible. */
  selectedChatId: string | null;
  /** Item links: the item each chat shows, and the item folders the user collapsed. */
  itemLinks?: { refs: ReadonlyMap<string, ItemRef>; collapsed: ReadonlySet<string> };
  /** Show projects that hold no chats (hidden by default; extended to every kind). */
  showEmpty?: boolean;
  /** Show board projects whose work is all finished (hidden by default). */
  showFinished?: boolean;
}

/** A board project whose work is all finished on the board. A stale or archived one is handled
 *  by those rules instead, and a folder project never is. */
export function isFinished(p: ProjectNode): boolean {
  return p.board?.completed === true && !p.board.stale && !p.archived;
}

/** A chat that keeps a finished project in view: running, waiting on the user, or pinned. */
function wantsUser(r: SidebarRow): boolean {
  return isBusy(r.status) || r.attention !== null || r.pinned || r.unread;
}

/** Per-row drawn depth and the children each row holds, for one group's held rows. */
export function nestRows(rows: SidebarRow[]): { order: { row: SidebarRow; depth: number }[]; childCount: Map<string, number> } {
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  const parentOf = (r: SidebarRow): string | null => {
    const p = r.parentChatId ? byId.get(r.parentChatId) : undefined;
    if (!p || p.groupProjectId !== r.groupProjectId || p.archived !== r.archived) return null;
    return p.chatId;
  };
  // Cut cycles: a row whose ancestor chain returns to itself is drawn at the top level.
  const effectiveParent = new Map<string, string | null>();
  for (const r of rows) {
    let p = parentOf(r);
    const seen = new Set([r.chatId]);
    let cur = p;
    while (cur) {
      if (seen.has(cur)) {
        p = null;
        break;
      }
      seen.add(cur);
      const next = byId.get(cur);
      cur = next ? parentOf(next) : null;
    }
    effectiveParent.set(r.chatId, p);
  }
  const children = new Map<string, SidebarRow[]>();
  const roots: SidebarRow[] = [];
  for (const r of rows) {
    const p = effectiveParent.get(r.chatId) ?? null;
    if (p) {
      const list = children.get(p) ?? [];
      list.push(r);
      children.set(p, list);
    } else roots.push(r);
  }
  const order: { row: SidebarRow; depth: number }[] = [];
  const walk = (r: SidebarRow, depth: number) => {
    order.push({ row: r, depth: Math.min(depth, MAX_DEPTH) });
    for (const c of children.get(r.chatId) ?? []) walk(c, depth + 1);
  };
  for (const r of roots) walk(r, 0);
  const childCount = new Map<string, number>();
  for (const [k, v] of children) childCount.set(k, v.length);
  return { order, childCount };
}

function projectChildren(projects: ProjectNode[]): { roots: ProjectNode[]; kids: Map<string, ProjectNode[]> } {
  const ids = new Set(projects.map((p) => p.id));
  const kids = new Map<string, ProjectNode[]>();
  const roots: ProjectNode[] = [];
  for (const p of projects) {
    if (p.parentId && ids.has(p.parentId) && p.parentId !== p.id) {
      const list = kids.get(p.parentId) ?? [];
      list.push(p);
      kids.set(p.parentId, list);
    } else roots.push(p);
  }
  return { roots, kids };
}

export function buildList(projects: ProjectNode[], rows: SidebarRow[], o: BuildOptions): ListItem[] {
  const h = o.compact ? HEIGHTS.compact : HEIGHTS.normal;
  if (o.view === 'activity') {
    return rows
      .filter((r) => !r.archived)
      .slice()
      .sort((a, b) => b.updatedAt - a.updatedAt || (a.chatId < b.chatId ? -1 : 1))
      .map((row) => ({
        kind: 'chat' as const,
        key: row.chatId,
        row,
        depth: 0,
        height: h.twoLine,
        children: 0,
        expanded: false,
        twoLine: true,
      }));
  }
  const archivedView = o.view === 'archived';
  const shown = rows.filter((r) => r.archived === archivedView);
  const byGroup = new Map<string, SidebarRow[]>();
  for (const r of shown) {
    const list = byGroup.get(r.groupProjectId) ?? [];
    list.push(r);
    byGroup.set(r.groupProjectId, list);
  }
  const selectedGroup = o.selectedChatId ? rows.find((r) => r.chatId === o.selectedChatId)?.groupProjectId : undefined;
  const { roots, kids } = projectChildren(projects);
  const byId = new Map(projects.map((p) => [p.id, p]));

  // Which groups to draw: in the archived view only those holding an archived chat (or archived
  // themselves, or with such a descendant). A stale board group with no chats is dropped. In the
  // projects view a group with no chats (its host count and the rows held both zero) and no shown
  // child is hidden unless it is pinned or holds the open chat; "Show N empty projects" brings
  // those back. A finished board project is hidden whether or not it holds chats, unless it is
  // pinned, holds the open chat or a chat that wants the user, or has a shown child; "Show N
  // finished projects" brings those back. Each hidden group is counted under one row only.
  const memo = new Map<string, boolean>();
  const base = (p: ProjectNode, guard: Set<string> = new Set()): boolean => {
    const known = memo.get(p.id);
    if (known !== undefined) return known;
    if (guard.has(p.id)) return false;
    guard.add(p.id);
    const own = (byGroup.get(p.id)?.length ?? 0) > 0;
    const child = (kids.get(p.id) ?? []).some((k) => base(k, guard));
    let v: boolean;
    if (archivedView) v = own || child || p.archived;
    else if (p.archived) v = false;
    else if (p.board?.stale) v = own || child || p.id === selectedGroup;
    else if (isFinished(p)) v = child || p.pinned || p.id === selectedGroup || (byGroup.get(p.id) ?? []).some(wantsUser);
    else v = own || child || p.chatCount > 0 || p.pinned || p.id === selectedGroup;
    memo.set(p.id, v);
    return v;
  };
  /** Hidden only for being finished: shown when the user asks for finished projects. */
  const finishedOnly = (p: ProjectNode) => !archivedView && isFinished(p) && !base(p);
  /** Hidden only for being empty: shown when the user asks for empty projects. */
  const emptyOnly = (p: ProjectNode) => !archivedView && !p.archived && !p.board?.stale && !isFinished(p) && !base(p);
  const visibleWith = (p: ProjectNode, showEmpty: boolean, showFinished: boolean): boolean =>
    base(p) || (showEmpty && emptyOnly(p)) || (showFinished && finishedOnly(p));
  const visible = (p: ProjectNode): boolean => visibleWith(p, o.showEmpty === true, o.showFinished === true);

  const out: ListItem[] = [];
  const emitGroup = (p: ProjectNode, depth: number, path: Set<string>) => {
    if (path.has(p.id) || !visible(p)) return;
    const collapsed = p.collapsed && p.id !== selectedGroup;
    const subtitle = headerSubtitle(p, byId);
    out.push({
      kind: 'header',
      key: `h:${p.id}`,
      project: p,
      depth: Math.min(depth, MAX_DEPTH),
      height: subtitle ? h.headerSubtitle : h.header,
      collapsed,
      subtitle,
    });
    if (collapsed) return;
    const next = new Set(path).add(p.id);
    for (const k of kids.get(p.id) ?? []) emitGroup(k, depth + 1, next);
    const { order, childCount } = nestRows(byGroup.get(p.id) ?? []);
    const hidden = new Set<string>();
    for (const e of arrange(order, o.itemLinks?.refs, o.itemLinks?.collapsed, MAX_DEPTH, o.selectedChatId)) {
      if (e.type === 'folder') {
        out.push({ kind: 'itemFolder', key: `i:${p.id}:${e.itemId}`, itemId: e.itemId, title: e.title, members: e.members, depth: e.depth, height: h.chat, collapsed: e.collapsed });
        continue;
      }
      const { row, depth: d } = e;
      if (row.parentChatId && hidden.has(row.parentChatId)) {
        hidden.add(row.chatId);
        continue;
      }
      const n = childCount.get(row.chatId) ?? 0;
      const expanded = n > 0 && o.expandedChats.has(row.chatId);
      if (n > 0 && !expanded) hidden.add(row.chatId);
      out.push({
        kind: 'chat',
        key: row.chatId,
        row,
        depth: d,
        height: h.chat,
        children: n,
        expanded,
        twoLine: false,
        ...(e.chip ? { itemChip: e.chip } : {}),
      });
    }
  };

  const pinned = roots.filter((p) => p.pinned && visible(p));
  const rest = roots.filter((p) => !p.pinned && visible(p));
  for (const p of pinned) emitGroup(p, 0, new Set());
  if (pinned.length > 0 && rest.length > 0) out.push({ kind: 'pinBreak', key: 'pin-break', height: h.pinBreak });
  for (const p of rest) emitGroup(p, 0, new Set());

  // Rows whose group is not in the project list (not loaded yet) go under a plain fallback group.
  const known = new Set(projects.map((p) => p.id));
  const orphans = shown.filter((r) => !known.has(r.groupProjectId));
  if (orphans.length > 0) {
    for (const row of orphans) {
      out.push({ kind: 'chat', key: row.chatId, row, depth: 0, height: h.chat, children: 0, expanded: false, twoLine: false });
    }
  }

  // Count the groups each row would bring back, walking the tree as it would draw with that row's
  // groups shown and the other row as it stands (a group under an archived, stale or still hidden
  // parent never draws, so it is not counted).
  if (!archivedView) {
    const count = (hiddenFor: (p: ProjectNode) => boolean, showEmpty: boolean, showFinished: boolean) => {
      let n = 0;
      const walk = (p: ProjectNode, path: Set<string>) => {
        if (path.has(p.id) || !visibleWith(p, showEmpty, showFinished)) return;
        if (hiddenFor(p)) n += 1;
        const next = new Set(path).add(p.id);
        for (const k of kids.get(p.id) ?? []) walk(k, next);
      };
      for (const p of roots) walk(p, new Set());
      return n;
    };
    const empty = count(emptyOnly, true, o.showFinished === true);
    if (empty > 0) out.push({ kind: 'emptyToggle', key: 'empty-toggle', height: h.chat, count: empty, shown: o.showEmpty === true });
    const finished = count(finishedOnly, o.showEmpty === true, true);
    if (finished > 0) out.push({ kind: 'finishedToggle', key: 'finished-toggle', height: h.chat, count: finished, shown: o.showFinished === true });
  }
  return out;
}

/** The muted line under a header: where a board project's chats
 *  start, shown once on the header, that it is no longer on the board, or that its work is done. */
export function headerSubtitle(p: ProjectNode, byId: ReadonlyMap<string, ProjectNode>): string | null {
  if (p.board?.stale) return 'Not on the board';
  if (isFinished(p)) return 'Finished';
  if (p.kind === 'board' && p.primaryFolderProjectId) return byId.get(p.primaryFolderProjectId)?.path ?? null;
  return null;
}

/** Ancestors of a chat among held rows, nearest first (to auto-expand the open chat's parents). */
export function chatAncestors(rows: SidebarRow[], chatId: string): string[] {
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  const out: string[] = [];
  const seen = new Set([chatId]);
  let cur = byId.get(chatId)?.parentChatId ?? null;
  while (cur && !seen.has(cur) && byId.has(cur)) {
    out.push(cur);
    seen.add(cur);
    cur = byId.get(cur)!.parentChatId;
  }
  return out;
}

/** Chat ids in the order the list draws them. Rows in collapsed groups or under collapsed parents
 *  are not drawn, so they are skipped (next/previous chat). */
export function visibleChatOrder(items: readonly ListItem[]): string[] {
  return items.flatMap((i) => (i.kind === 'chat' ? [i.row.chatId] : []));
}

/** Ctrl+Tab (1) and Ctrl+Shift+Tab (-1): the next or previous drawn chat, wrapping. From a chat
 *  that is not drawn (or none), next is the first and previous the last. */
export function stepChat(order: readonly string[], current: string | null, dir: 1 | -1): string | null {
  if (order.length === 0) return null;
  const i = current ? order.indexOf(current) : -1;
  if (i < 0) return dir === 1 ? order[0]! : order[order.length - 1]!;
  return order[(i + dir + order.length) % order.length]!;
}
