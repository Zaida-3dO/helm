// Drag and drop in the sidebar, as pure decisions.
//
// A drag starts after 4 px of movement with the primary button, never in the Activity view.
// Where it lands:
// - a chat onto a group header: show it under that group;
// - a chat onto the upper half of another chat: place it before that chat, at that chat's level
//   (its group, its parent chat and its pin state), so the drop never asks the host to reorder
//   across levels;
// - a chat onto the lower half of another chat: nest it under that chat;
// - a level the chat may not take (under itself or its own started chats, or deeper than three
//   levels) is not offered;
// - a project header onto the upper half of another header: reorder before it (same parent and
//   pin group), onto the lower half: nest under it.

import type { SidebarRow } from '../api/projects.ts';
import { MAX_DEPTH } from './model.ts';

export const DRAG_THRESHOLD_PX = 4;

export type DragSource = { kind: 'chat'; chatId: string; groupId: string; pinned: boolean } | { kind: 'project'; projectId: string; parentId: string | null; pinned: boolean };

export type DropTarget =
  | { kind: 'header'; projectId: string; parentId: string | null; pinned: boolean; lowerHalf: boolean }
  | { kind: 'chat'; chatId: string; groupId: string; pinned: boolean; lowerHalf: boolean; parentChatId?: string | null; levelOk?: boolean; nestOk?: boolean };

export type DropAction =
  | { kind: 'chat-to-group'; chatId: string; groupId: string }
  | { kind: 'chat-before'; chatId: string; beforeChatId: string; groupId: string; moveGroup: boolean; parentChatId: string | null; pinned: boolean }
  | { kind: 'chat-nest'; chatId: string; parentChatId: string; groupId: string; moveGroup: boolean }
  | { kind: 'project-before'; projectId: string; beforeProjectId: string }
  | { kind: 'project-nest'; projectId: string; parentId: string };

export function passedThreshold(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD_PX;
}

/** Whether `chatId` (with the chats started from it) may sit directly under `parentId` (`null` =
 *  top level): not under itself or one of its own started chats, and within three levels. */
export function canSitUnder(rows: readonly SidebarRow[], chatId: string, parentId: string | null): boolean {
  if (parentId === null) return true;
  if (parentId === chatId) return false;
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  let depth = 0;
  const seen = new Set<string>();
  for (let cur: string | null | undefined = byId.get(parentId)?.parentChatId; cur; cur = byId.get(cur)?.parentChatId) {
    if (cur === chatId) return false;
    if (seen.has(cur)) break;
    seen.add(cur);
    depth++;
  }
  const height = (id: string, guard = 0): number => {
    if (guard > MAX_DEPTH + 2) return 0;
    const kids = rows.filter((r) => r.parentChatId === id);
    return kids.length === 0 ? 0 : 1 + Math.max(...kids.map((k) => height(k.chatId, guard + 1)));
  };
  return depth + 1 + height(chatId) <= MAX_DEPTH;
}

/** What a drop would do, or null when it would change nothing or is not allowed. */
export function dropAction(src: DragSource, t: DropTarget): DropAction | null {
  if (src.kind === 'chat') {
    if (t.kind === 'header') {
      if (t.projectId === src.groupId) return null;
      return { kind: 'chat-to-group', chatId: src.chatId, groupId: t.projectId };
    }
    if (t.chatId === src.chatId) return null;
    const moveGroup = t.groupId !== src.groupId;
    if (t.lowerHalf && t.nestOk === false) return null;
    if (t.lowerHalf) return { kind: 'chat-nest', chatId: src.chatId, parentChatId: t.chatId, groupId: t.groupId, moveGroup };
    if (t.levelOk === false) return null;
    return { kind: 'chat-before', chatId: src.chatId, beforeChatId: t.chatId, groupId: t.groupId, moveGroup, parentChatId: t.parentChatId ?? null, pinned: t.pinned };
  }
  if (t.kind !== 'header' || t.projectId === src.projectId) return null;
  if (t.lowerHalf) return { kind: 'project-nest', projectId: src.projectId, parentId: t.projectId };
  if (t.parentId !== src.parentId || t.pinned !== src.pinned) return null;
  return { kind: 'project-before', projectId: src.projectId, beforeProjectId: t.projectId };
}
