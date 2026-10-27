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
// Projects with no chats never show in the projects view (a pinned one, the open chat's project and
// a parent of a shown project excepted). A finished board project (all its work done on the board)
// shows only while it still holds chats, so none of them become unreachable.

import type { ProjectNode, SidebarRow, SidebarView } from '../api/projects.ts';
import { arrange, type ItemRef } from '../itemlinks/folders.ts';
import { isBusy } from './status.ts';

export const MAX_DEPTH = 2;

/** Virtual heights including the 2 px gap. */
export const HEIGHTS = {
  normal: { chat: 36, twoLine: 56, header: 40, headerSubtitle: 56, pinBreak: 13 },
  compact: { chat: 28, twoLine: 38, header: 34, headerSubtitle: 50, pinBreak: 13 },
} as const;

/** Where an item sits in its project's block: the header and its chats draw as one tinted block. */
export type BlockPos = 'only' | 'first' | 'mid' | 'last';

/** What a collapsed group still has going on inside it: chats running and chats with an unread result. */
export interface GroupMarks {
  running: number;
  unread: number;
}

export type ListItem = (
  | { kind: 'header'; key: string; project: ProjectNode; depth: number; height: number; collapsed: boolean; subtitle: string | null; marks: GroupMarks | null }
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
  | { kind: 'itemFolder'; key: string; groupId: string; itemId: string; title: string; members: number; depth: number; height: number; collapsed: boolean }
  | { kind: 'pinBreak'; key: string; height: number }
) & { block?: BlockPos };

export interface BuildOptions {
  view: SidebarView;
  compact: boolean;
  /** Parents the user expanded (parents are collapsed by default). */
  expandedChats: ReadonlySet<string>;
  /** The open chat: its group and ancestors stay visible. */
  selectedChatId: string | null;
  /** Draw the open chat's project, and the projects it sits in, expanded even where they were left
   *  collapsed, so a chat opened from search, a link or a notification can be seen. The sidebar
   *  turns this off once the user collapses one of them: their collapse always wins. */
  revealOpenChat?: boolean;
  /** Item links: the item each chat shows, and the item folders the user collapsed. */
  itemLinks?: { refs: ReadonlyMap<string, ItemRef>; collapsed: ReadonlySet<string> };
}

/** The host's order for chats in one group: pinned first, then dragged position, then newest first.
 *  Equal rows compare as 0, so a stable sort keeps the order they came in. */
export function chatOrder(a: SidebarRow, b: SidebarRow): number {
  return Number(b.pinned) - Number(a.pinned) || (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER) || b.createdAt - a.createdAt;
}

/** A board project whose work is all finished on the board. A stale or archived one is handled
 *  by those rules instead, and a folder project never is. */
export function isFinished(p: ProjectNode): boolean {
  return p.board?.completed === true && !p.board.stale && !p.archived;
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
  // The host's order within a group: pinned first, then by dragged position, then newest first. The
  // rows may have arrived in another order (a drag changes positions after they were loaded); equal
  // rows keep the order they came in.
  for (const list of byGroup.values()) list.sort(chatOrder);
  const selectedGroup = o.selectedChatId ? rows.find((r) => r.chatId === o.selectedChatId)?.groupProjectId : undefined;
  const { roots, kids } = projectChildren(projects);
  const byId = new Map(projects.map((p) => [p.id, p]));
  const revealed = o.revealOpenChat ? projectsHolding(byId, selectedGroup) : new Set<string>();

  // Which groups to draw: in the archived view only those holding an archived chat (or archived
  // themselves, or with such a descendant). A stale board group with no chats is dropped. In the
  // projects view a group with no chats (its host count and the rows held both zero) and no shown
  // child is hidden unless it is pinned or holds the open chat. A finished board project follows
  // the same rule, so it stays in view for as long as it holds chats.
  const memo = new Map<string, boolean>();
  const visible = (p: ProjectNode, guard: Set<string> = new Set()): boolean => {
    const known = memo.get(p.id);
    if (known !== undefined) return known;
    if (guard.has(p.id)) return false;
    guard.add(p.id);
    const own = (byGroup.get(p.id)?.length ?? 0) > 0;
    const child = (kids.get(p.id) ?? []).some((k) => visible(k, guard));
    let v: boolean;
    if (archivedView) v = own || child || p.archived;
    else if (p.archived) v = false;
    else if (p.board?.stale) v = own || child || p.id === selectedGroup;
    else v = own || child || p.chatCount > 0 || p.pinned || p.id === selectedGroup;
    memo.set(p.id, v);
    return v;
  };

  // Running and unread chats inside a project and the projects nested under it, for its header
  // while it is collapsed.
  const groupMarks = (p: ProjectNode, path: Set<string> = new Set()): GroupMarks | null => {
    if (path.has(p.id)) return null;
    const next = new Set(path).add(p.id);
    let running = 0;
    let unread = 0;
    for (const r of byGroup.get(p.id) ?? []) {
      if (isBusy(r.status)) running += 1;
      else if (r.unread) unread += 1;
    }
    for (const k of kids.get(p.id) ?? []) {
      const sub = groupMarks(k, next);
      running += sub?.running ?? 0;
      unread += sub?.unread ?? 0;
    }
    return running + unread > 0 ? { running, unread } : null;
  };

  const out: ListItem[] = [];
  const emitGroup = (p: ProjectNode, depth: number, path: Set<string>) => {
    if (path.has(p.id) || !visible(p)) return;
    const collapsed = p.collapsed && !revealed.has(p.id);
    const subtitle = headerSubtitle(p, byId);
    out.push({
      kind: 'header',
      key: `h:${p.id}`,
      project: p,
      depth: Math.min(depth, MAX_DEPTH),
      height: subtitle ? h.headerSubtitle : h.header,
      collapsed,
      subtitle,
      marks: collapsed ? groupMarks(p) : null,
    });
    if (collapsed) return;
    const next = new Set(path).add(p.id);
    for (const k of kids.get(p.id) ?? []) emitGroup(k, depth + 1, next);
    const { order, childCount } = nestRows(byGroup.get(p.id) ?? []);
    const hidden = new Set<string>();
    for (const e of arrange(order, o.itemLinks?.refs, o.itemLinks?.collapsed, MAX_DEPTH, o.selectedChatId)) {
      if (e.type === 'folder') {
        out.push({ kind: 'itemFolder', key: `i:${p.id}:${e.itemId}`, groupId: p.id, itemId: e.itemId, title: e.title, members: e.members, depth: e.depth, height: h.chat, collapsed: e.collapsed });
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
  markBlocks(out);
  return out;
}

/** A project and every project it sits in. */
export function projectsHolding(byId: ReadonlyMap<string, ProjectNode>, projectId: string | undefined): Set<string> {
  const out = new Set<string>();
  for (let id = projectId; id && !out.has(id); id = byId.get(id)?.parentId ?? undefined) out.add(id);
  return out;
}

/** The project an item belongs to, for grouping neighbours into one block. */
function ownerOf(i: ListItem): string | null {
  return i.kind === 'header' ? i.project.id : i.kind === 'chat' ? i.row.groupProjectId : i.kind === 'itemFolder' ? i.groupId : null;
}

/** Mark each run of items that share a project as one block (first, mid and last), so the sidebar
 *  can draw a project's header and chats as a single tinted shape instead of separate pills. A
 *  header always starts a block; the activity view is not marked (its rows stay separate). */
function markBlocks(items: ListItem[]): void {
  items.forEach((it, k) => {
    const owner = ownerOf(it);
    if (owner === null) return;
    const above = k > 0 && it.kind !== 'header' && ownerOf(items[k - 1]!) === owner;
    const next = items[k + 1];
    const below = next !== undefined && next.kind !== 'header' && ownerOf(next) === owner;
    it.block = above ? (below ? 'mid' : 'last') : below ? 'first' : 'only';
  });
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
