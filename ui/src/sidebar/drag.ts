// Drag and drop in the sidebar, as pure decisions.
//
// A drag starts after 4 px of movement with the primary button, never in the Activity view.
// Where it lands:
// - a chat onto a group header: show it under that group;
// - a chat onto the upper half of another chat: place it before that chat (in that chat's group,
//   among chats of the same pin state);
// - a chat onto the lower half of another chat: nest it under that chat;
// - a project header onto the upper half of another header: reorder before it (same parent and
//   pin group), onto the lower half: nest under it.

export const DRAG_THRESHOLD_PX = 4;

export type DragSource = { kind: 'chat'; chatId: string; groupId: string; pinned: boolean } | { kind: 'project'; projectId: string; parentId: string | null; pinned: boolean };

export type DropTarget =
  | { kind: 'header'; projectId: string; parentId: string | null; pinned: boolean; lowerHalf: boolean }
  | { kind: 'chat'; chatId: string; groupId: string; pinned: boolean; lowerHalf: boolean };

export type DropAction =
  | { kind: 'chat-to-group'; chatId: string; groupId: string }
  | { kind: 'chat-before'; chatId: string; beforeChatId: string; groupId: string; moveGroup: boolean }
  | { kind: 'chat-nest'; chatId: string; parentChatId: string; groupId: string; moveGroup: boolean }
  | { kind: 'project-before'; projectId: string; beforeProjectId: string }
  | { kind: 'project-nest'; projectId: string; parentId: string };

export function passedThreshold(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= DRAG_THRESHOLD_PX;
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
    if (t.lowerHalf) return { kind: 'chat-nest', chatId: src.chatId, parentChatId: t.chatId, groupId: t.groupId, moveGroup };
    // No pin or unpin by drop: a reorder stays within the pin state.
    if (!moveGroup && t.pinned !== src.pinned) return null;
    return { kind: 'chat-before', chatId: src.chatId, beforeChatId: t.chatId, groupId: t.groupId, moveGroup };
  }
  if (t.kind !== 'header' || t.projectId === src.projectId) return null;
  if (t.lowerHalf) return { kind: 'project-nest', projectId: src.projectId, parentId: t.projectId };
  if (t.parentId !== src.parentId || t.pinned !== src.pinned) return null;
  return { kind: 'project-before', projectId: src.projectId, beforeProjectId: t.projectId };
}
