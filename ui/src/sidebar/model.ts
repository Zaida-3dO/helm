// The sidebar's list model, as pure functions.
//
// Projects view: one grouped tree. Top-level projects in host order (pinned first), a pin break
// between pinned and unpinned top-level groups, and under each expanded group first its child
// projects (one level deeper each, at most depth index 2), then its chats. Chats nest under chats
// only when the parent is held, in the same group and the same archived state; anything else is
// drawn at the top level, cycles are cut, and deeper records render at depth index 2.
// Activity view: every held chat, newest activity first, as two-line rows; no headers or nesting.
// The archived list offers the same Projects / Activity pair: archivedProjects groups only archived
// chats under the projects that hold one (or are themselves archived), archivedActivity lists them
// newest activity first.
// Projects with no chats never show in the projects view (a pinned one, the open chat's project and
// a parent of a shown project excepted). A finished board project (all its work done on the board)
// shows only while it still holds chats, so none of them become unreachable.
// The sidebar shows boards: a folder project is only where chats run (Settings → Projects lists
// them), so one shows here only while chats are still shown under it (before they are moved onto a
// board, or when no task board is connected), never just for being pinned or open.

import type { ProjectNode, SidebarRow } from '../api/projects.ts';
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

/** The expanded chats to draw: each chat's own open state (it rides on the chat's row, so
 *  the chat is expanded the same on every PC that views it) plus the window's ephemeral
 *  auto-expansions that bring the open chat into view, minus any chat the person shut in
 *  this window (which stays shut until its shut write answers). */
export function effectiveExpanded(rows: readonly SidebarRow[], ephemeral: ReadonlySet<string>, shut: ReadonlySet<string>): Set<string> {
  const out = new Set<string>();
  for (const r of rows) if (r.expanded && !shut.has(r.chatId)) out.add(r.chatId);
  for (const id of ephemeral) if (!shut.has(id)) out.add(id);
  return out;
}

/** Drop the window's local toggle bridges once the chat's own record carries the state this
 *  window asked for: an open in `ephemeral` rides on until its row says open, a shut in `shut`
 *  until its row says shut. After that the bridge is dead weight, and keeping it would ignore
 *  the same toggle from another PC forever. `aid` is the open chat's ancestors: a shut one
 *  stays shut while that chat is open (the person's collapse outranks the auto-expansion), and
 *  rows this window does not hold say nothing either way — both bridges ride on. Returns the
 *  new sets and whether anything was dropped. */
export function pruneToggled(rows: readonly SidebarRow[], ephemeral: ReadonlySet<string>, shut: ReadonlySet<string>, aid: ReadonlySet<string>): { ephemeral: Set<string>; shut: Set<string>; changed: boolean } {
  const openOf = new Map(rows.map((r) => [r.chatId, r.expanded === true]));
  const e = new Set([...ephemeral].filter((id) => openOf.get(id) !== true));
  const s = new Set([...shut].filter((id) => !(openOf.get(id) === false && !aid.has(id))));
  return { ephemeral: e, shut: s, changed: e.size !== ephemeral.size || s.size !== shut.size };
}

/** What a collapsed group still has going on inside it: chats running, chats with an unread
 *  result, chats waiting on you or flagged as possibly needing you, chats holding an unsent
 *  draft, chats that finished a reply nobody has opened, and chats idle apart from background
 *  work still running. The header folds these into one status, same priority as a chat row. */
export interface GroupMarks {
  running: number;
  unread: number;
  flagged: number;
  draft: number;
  done: number;
  lingering: number;
}

/** The one status a collapsed header shows, in the same priority order as a row. */
export function groupStatus(m: GroupMarks): 'running' | 'flagged' | 'draft' | 'done' | 'lingering' | 'unread' | null {
  if (m.running > 0) return 'running';
  if (m.flagged > 0) return 'flagged';
  if (m.draft > 0) return 'draft';
  if (m.done > 0) return 'done';
  if (m.lingering > 0) return 'lingering';
  if (m.unread > 0) return 'unread';
  return null;
}

export type ListItem = (
  | {
      kind: 'header';
      key: string;
      project: ProjectNode;
      depth: number;
      height: number;
      collapsed: boolean;
      subtitle: string | null;
      marks: GroupMarks | null;
      /** A board with no folder yet: the header offers "Set folder". */
      needsFolder: boolean;
    }
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
      /** The task-board item this chat is linked to. No longer drawn on the row itself
       *  (the row's menu opens the item), but the folder grouping reads it. */
      itemChip?: ItemRef;
      /** The chat finished a reply the person has not opened (from the attention summary):
       *  the row's one status icon, animated. */
      finished?: boolean;
      /** Totalled over every descendant of a chat with children: how many run, may need the
       *  person, hold an unsent draft or finished unread. A collapsed parent folds these
       *  into its own status; an expanded one shows its children as rows instead. */
      childRunning?: number;
      childFlagged?: number;
      childDraft?: number;
      childDone?: number;
    }
  | { kind: 'itemFolder'; key: string; groupId: string; itemId: string; title: string; members: number; depth: number; height: number; collapsed: boolean }
  | { kind: 'pinBreak'; key: string; height: number }
) & { block?: BlockPos };

/** What the list draws: the host's projects/activity/archived paging view plus the archived
 *  list's own Projects / Activity choice. Paging stays keyed on the host's three views. */
export type ListView = 'projects' | 'activity' | 'archivedProjects' | 'archivedActivity';

export interface BuildOptions {
  view: ListView;
  compact: boolean;
  /** Parents the user expanded (parents are collapsed by default). */
  expandedChats: ReadonlySet<string>;
  /** The open chat: its group and ancestors stay visible. */
  selectedChatId: string | null;
  /** The open chat's project, and the projects it sits in, draw expanded even where they were left
   *  collapsed, so a chat opened from search, a link or a notification can be seen. These are the
   *  ones the user collapsed while that chat was open: they stay collapsed, as the user asked. */
  keptShut?: ReadonlySet<string>;
  /** Chats waiting on the user or flagged by the idle check: a collapsed group counts them. */
  flagged?: ReadonlySet<string>;
  /** Chats that finished a reply the person has not opened: a collapsed group folds them as done. */
  finished?: ReadonlySet<string>;
  /** Closed projects: their chats are never drawn, not even as loose rows. */
  closed?: ReadonlySet<string>;
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

/** Per-row drawn depth and the children each row holds, for one group's held rows. With the
 *  flagged and finished sets it also totals, under each chat, the descendants that run, may need
 *  the person, hold an unsent draft or finished unread (for a collapsed parent's one status). */
export function nestRows(rows: SidebarRow[], flagged?: ReadonlySet<string>, finished?: ReadonlySet<string>): {
  order: { row: SidebarRow; depth: number }[];
  childCount: Map<string, number>;
  childMarks: Map<string, { running: number; flagged: number; draft: number; done: number }>;
} {
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
  // What folds into each collapsed parent: every descendant (the cycle cut above keeps this a
  // forest), counted once each for running, the flagged set, held drafts and unread completions.
  const childMarks = new Map<string, { running: number; flagged: number; draft: number; done: number }>();
  const marksUnder = (id: string): { running: number; flagged: number; draft: number; done: number } => {
    const hit = childMarks.get(id);
    if (hit) return hit;
    let running = 0;
    let needPerson = 0;
    let draft = 0;
    let done = 0;
    for (const c of children.get(id) ?? []) {
      const sub = marksUnder(c.chatId);
      running += sub.running + (isBusy(c.status) ? 1 : 0);
      needPerson += sub.flagged + (flagged?.has(c.chatId) ? 1 : 0);
      draft += sub.draft + (c.hasDraft ? 1 : 0);
      done += sub.done + (finished?.has(c.chatId) ? 1 : 0);
    }
    const m = { running, flagged: needPerson, draft, done };
    childMarks.set(id, m);
    return m;
  };
  for (const r of rows) marksUnder(r.chatId);
  return { order, childCount, childMarks };
}

/** The marks that ride the title line, in draw order: the muted bell right after the title, the
 *  shared-from disc, and the children count (collapsed parents only — an expanded one reaches its
 *  children through the chevron). These stay put on hover; the far-right cluster changes there. */
export function titleCluster(b: { muted: boolean; shared: boolean; children: number; expanded: boolean }): ('mute' | 'shared' | 'children')[] {
  const out: ('mute' | 'shared' | 'children')[] = [];
  if (b.muted) out.push('mute');
  if (!b.expanded && b.children > 0) out.push('children');
  if (b.shared) out.push('shared');
  return out;
}

/** The far-right cluster, left to right: state text, queue, the one status icon, age last.
 *  The age sits hard right and no icon ever pushes it in; on hover the age and status hide and
 *  the row's action buttons take their place, while the queue stays. */
export function rightCluster(b: { chip: boolean; queue: boolean; status: boolean }): ('chip' | 'queue' | 'status' | 'age')[] {
  const out: ('chip' | 'queue' | 'status' | 'age')[] = [];
  if (b.chip) out.push('chip');
  if (b.queue) out.push('queue');
  if (b.status) out.push('status');
  out.push('age');
  return out;
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
  const closed = o.closed;
  const archivedActivity = o.view === 'archivedActivity';
  if (o.view === 'activity' || archivedActivity) {
    return rows
      .filter((r) => r.archived === archivedActivity && !closed?.has(r.groupProjectId))
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
        ...(o.finished?.has(row.chatId) ? { finished: true } : {}),
      }));
  }
  const archivedView = o.view === 'archivedProjects';
  const shown = rows.filter((r) => r.archived === archivedView && !closed?.has(r.groupProjectId));
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
  const revealed = projectsHolding(byId, selectedGroup);

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
    else if (p.kind !== 'board') v = own || child || p.chatCount > 0;
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
    let flagged = 0;
    let lingering = 0;
    let draft = 0;
    let done = 0;
    for (const r of byGroup.get(p.id) ?? []) {
      if (isBusy(r.status)) running += 1;
      else if (o.finished?.has(r.chatId)) done += 1;
      else if (r.unread) unread += 1;
      else if (r.attention === 'lingering') lingering += 1;
      if (o.flagged?.has(r.chatId)) flagged += 1;
      if (r.hasDraft) draft += 1;
    }
    for (const k of kids.get(p.id) ?? []) {
      const sub = groupMarks(k, next);
      running += sub?.running ?? 0;
      unread += sub?.unread ?? 0;
      flagged += sub?.flagged ?? 0;
      draft += sub?.draft ?? 0;
      done += sub?.done ?? 0;
      lingering += sub?.lingering ?? 0;
    }
    return running + unread + flagged + draft + done + lingering > 0 ? { running, unread, flagged, draft, done, lingering } : null;
  };

  const out: ListItem[] = [];
  const emitGroup = (p: ProjectNode, depth: number, path: Set<string>) => {
    if (path.has(p.id) || !visible(p)) return;
    const collapsed = p.collapsed && (!revealed.has(p.id) || o.keptShut?.has(p.id) === true);
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
      needsFolder: boardNeedsFolder(p, byId),
    });
    if (collapsed) return;
    const next = new Set(path).add(p.id);
    for (const k of kids.get(p.id) ?? []) emitGroup(k, depth + 1, next);
    const { order, childCount, childMarks } = nestRows(byGroup.get(p.id) ?? [], o.flagged, o.finished);
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
      const marks = n > 0 ? childMarks.get(row.chatId) : undefined;
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
        ...(o.finished?.has(row.chatId) ? { finished: true } : {}),
        ...(marks ? { childRunning: marks.running, childFlagged: marks.flagged, childDraft: marks.draft, childDone: marks.done } : {}),
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
      out.push({ kind: 'chat', key: row.chatId, row, depth: 0, height: h.chat, children: 0, expanded: false, twoLine: false, ...(o.finished?.has(row.chatId) ? { finished: true } : {}) });
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

/** An open board with no usable folder: its new chats have nowhere to start until one is set. */
export function boardNeedsFolder(p: ProjectNode, byId: ReadonlyMap<string, ProjectNode>): boolean {
  if (p.kind !== 'board' || p.archived || p.board?.stale) return false;
  const folder = p.primaryFolderProjectId ? byId.get(p.primaryFolderProjectId) : undefined;
  return !folder || folder.archived || !folder.path;
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
