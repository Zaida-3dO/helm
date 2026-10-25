// The sidebar's actions, shared by the menus, drag and drop, hover buttons and shortcuts. Moves,
// nests, reorders and archives push an undo entry (undo.ts). Every action reports failures through
// `onError` as the host's own sentence; nothing here waits on the board.

import type { Api } from '../api/client.ts';
import type { ChatOps } from '../api/ops.ts';
import type { ChatOrganizeArgs, ProjectNode, ProjectOps, SidebarRow } from '../api/projects.ts';
import type { SidebarData } from './data.ts';
import { chatOrder } from './model.ts';
import { UndoStack, type UndoEntry } from './undo.ts';
import { toast } from '../ui/toasts.ts';
import type { ConfirmSpec } from '../ui/index.ts';

export interface ActionDeps {
  api: Api;
  pops: ProjectOps;
  ops: ChatOps;
  data: SidebarData;
  undo: UndoStack;
  onError: (message: string) => void;
}

function message(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

/** What archiving needs to know about a chat: the top bar has a chat record, the sidebar a row. */
export type ArchivableChat = Pick<SidebarRow, 'chatId' | 'title'>;

export class SidebarActions {
  private readonly d: ActionDeps;

  constructor(d: ActionDeps) {
    this.d = d;
  }

  private async run<T>(f: () => Promise<T>): Promise<T | undefined> {
    try {
      return await f();
    } catch (e) {
      this.d.onError(message(e));
      return undefined;
    }
  }

  private async attempt(f: () => Promise<unknown>): Promise<string | null> {
    try {
      await f();
      return null;
    } catch (e) {
      return message(e);
    }
  }

  private async organize(a: ChatOrganizeArgs): Promise<void> {
    const rows = await this.d.pops.organize(a);
    this.d.data.mergeRows(rows);
  }

  /** The current placement of a chat, to undo a move back to it. */
  private placement(row: SidebarRow): ChatOrganizeArgs {
    return { chatId: row.chatId, groupProjectId: row.groupProjectId, parentChatId: row.parentChatId };
  }

  /** Apply `a`, with an undo entry that puts the chat back where it was. */
  async move(row: SidebarRow, a: ChatOrganizeArgs, label: string): Promise<void> {
    const back = this.placement(row);
    const ok = await this.run(() => this.organize(a).then(() => true));
    if (!ok) return;
    this.d.undo.push({
      label,
      undo: () => this.organize(back),
      redo: () => this.organize(a),
    });
  }

  moveTo(row: SidebarRow, groupProjectId: string): Promise<void> {
    // A chat moved to another group leaves its parent behind.
    return this.move(row, { chatId: row.chatId, groupProjectId, parentChatId: null }, 'Move chat');
  }

  nestUnder(row: SidebarRow, parentChatId: string, groupProjectId?: string): Promise<void> {
    return this.move(row, { chatId: row.chatId, parentChatId, ...(groupProjectId ? { groupProjectId } : {}) }, 'Nest chat');
  }

  topLevel(row: SidebarRow): Promise<void> {
    return this.move(row, { chatId: row.chatId, parentChatId: null }, 'Move chat to top level');
  }

  /** Drop `row` before another chat, at that chat's level: its group (when different), its parent
   *  chat and its pin state travel with the move, so the host reorders among true siblings. */
  reorderBefore(row: SidebarRow, beforeChatId: string, level: { parentChatId: string | null; pinned: boolean; groupProjectId?: string }): Promise<void> {
    // Undo puts the chat back in its exact slot: before the sibling that followed it, or last. The
    // slot is decided when the undo runs: that sibling may have left the level since (pinned,
    // archived, moved, deleted), and then the chat goes last instead of the undo being refused.
    const atOldLevel = (r: SidebarRow) => r.groupProjectId === row.groupProjectId && r.pinned === row.pinned && r.parentChatId === row.parentChatId && r.archived === row.archived;
    const old = this.d.data.getSnapshot().rows.filter(atOldLevel).sort(chatOrder);
    const nextId = old[old.findIndex((r) => r.chatId === row.chatId) + 1]?.chatId;
    const back = async () => {
      const level: ChatOrganizeArgs = { ...this.placement(row), pinned: row.pinned };
      const stays = nextId !== undefined && this.d.data.getSnapshot().rows.some((r) => r.chatId === nextId && atOldLevel(r));
      if (!stays) return this.organize({ ...level, toEnd: true });
      try {
        await this.organize({ ...level, beforeChatId: nextId });
      } catch {
        await this.organize({ ...level, toEnd: true });
      }
    };
    const forward: ChatOrganizeArgs = { chatId: row.chatId, parentChatId: level.parentChatId, pinned: level.pinned, beforeChatId, ...(level.groupProjectId ? { groupProjectId: level.groupProjectId } : {}) };
    const levelChanged = !!level.groupProjectId || level.parentChatId !== row.parentChatId || level.pinned !== row.pinned;
    return this.run(async () => {
      await this.organize(forward);
      if (levelChanged) this.d.undo.push({ label: 'Move chat', undo: back, redo: () => this.organize(forward) });
    }).then(() => undefined);
  }

  pin(row: SidebarRow, pinned: boolean): Promise<void> {
    return this.run(() => this.organize({ chatId: row.chatId, pinned })).then(() => undefined);
  }

  async archive(row: ArchivableChat): Promise<void> {
    const ok = await this.run(() => this.d.ops.archive(row.chatId).then(() => true));
    if (!ok) return;
    await this.d.data.refetch([row.chatId]);
    this.archivedEntry(row);
  }

  /** Remember an archive so Ctrl+Z and the toast's Undo can revert it, and announce it. */
  private archivedEntry(row: ArchivableChat): void {
    const entry: UndoEntry = {
      label: 'Archive chat',
      undo: async () => {
        await this.d.ops.restore(row.chatId);
        await this.d.data.refetch([row.chatId]);
      },
      redo: async () => {
        await this.d.ops.archive(row.chatId);
        await this.d.data.refetch([row.chatId]);
      },
    };
    this.d.undo.push(entry);
    this.archivedToast(row.title || 'Untitled chat', entry);
  }

  async restore(row: SidebarRow): Promise<void> {
    await this.run(() => this.d.ops.restore(row.chatId));
    await this.d.data.refetch([row.chatId]);
  }

  async remove(row: SidebarRow): Promise<void> {
    await this.run(() => this.d.ops.remove(row.chatId));
    await this.d.data.refetch([row.chatId]);
  }

  async markUnread(row: SidebarRow): Promise<void> {
    await this.run(() => this.d.ops.markUnread(row.chatId));
    await this.d.data.refetch([row.chatId]);
  }

  async mute(row: SidebarRow, muted: boolean): Promise<void> {
    await this.run(() => this.d.ops.mute(row.chatId, muted));
    await this.d.data.refetch([row.chatId]);
  }

  /** Ops the host may not implement yet; until then it refuses them. */
  fork(row: SidebarRow): Promise<unknown> {
    const forkId = globalThis.crypto?.randomUUID?.() ?? `fork-${Date.now()}`;
    return this.run(() => this.d.api('chat_fork', { chatId: row.chatId, forkId }));
  }

  /** Rename for a dialog: waits for the host and answers with the reason it failed, or null. */
  renameChat(row: SidebarRow, title: string): Promise<string | null> {
    return this.attempt(() => this.d.api('chat_rename', { chatId: row.chatId, title }));
  }

  /** Edit a project for a dialog: waits for the host and answers with the reason it failed, or null. */
  editProject(p: ProjectNode, patch: Record<string, unknown>): Promise<string | null> {
    return this.attempt(async () => {
      try {
        this.d.data.project(await this.d.pops.update({ projectId: p.id, revision: p.revision, ...patch }));
      } catch (e) {
        void this.d.data.loadProjects();
        throw e;
      }
    });
  }

  // ---- projects ----

  async updateProject(p: ProjectNode, patch: Record<string, unknown>, undoPatch?: Record<string, unknown>, label = 'Edit project'): Promise<void> {
    const next = await this.run(() => this.d.pops.update({ projectId: p.id, revision: p.revision, ...patch }));
    if (!next) {
      void this.d.data.loadProjects();
      return;
    }
    this.d.data.project(next);
    if (undoPatch) {
      let cur = next;
      this.d.undo.push({
        label,
        undo: async () => {
          cur = await this.d.pops.update({ projectId: p.id, revision: cur.revision, ...undoPatch });
          this.d.data.project(cur);
        },
        redo: async () => {
          cur = await this.d.pops.update({ projectId: p.id, revision: cur.revision, ...patch });
          this.d.data.project(cur);
        },
      });
    }
  }

  nestProject(p: ProjectNode, parentId: string | null): Promise<void> {
    return this.updateProject(p, { parentId }, { parentId: p.parentId }, parentId ? 'Nest project' : 'Move project to top level');
  }

  /** Place a project before another one, or last when `beforeProjectId` is null. */
  reorderProject(p: ProjectNode, beforeProjectId: string | null): Promise<void> {
    return this.updateProject(p, { beforeProjectId });
  }

  async archiveProject(p: ProjectNode): Promise<void> {
    const ok = await this.run(() => this.d.pops.archive(p.id).then(() => true));
    if (!ok) return;
    void this.d.data.loadProjects();
    this.d.data.invalidatePaging();
    const entry: UndoEntry = {
      label: 'Archive project',
      undo: async () => {
        await this.d.pops.restore(p.id);
        await this.d.data.loadProjects();
      },
      redo: async () => {
        await this.d.pops.archive(p.id);
        await this.d.data.loadProjects();
      },
    };
    this.d.undo.push(entry);
    this.archivedToast(p.name, entry);
  }

  /** "Archived <title> · Undo": the button undoes this archive, not whatever was done last. */
  private archivedToast(title: string, entry: UndoEntry): void {
    toast.show({
      kind: 'undo',
      icon: 'archive',
      text: `Archived “${title}”`,
      action: { label: 'Undo', hint: 'Ctrl+Z', run: () => void this.run(() => this.d.undo.undoEntry(entry)) },
    });
  }

  async restoreProject(p: ProjectNode): Promise<void> {
    await this.run(() => this.d.pops.restore(p.id));
    void this.d.data.loadProjects();
  }

  async undo(): Promise<void> {
    await this.run(() => this.d.undo.undo());
  }

  async redo(): Promise<void> {
    await this.run(() => this.d.undo.redo());
  }
}

/** "Compact context": ask the chat's worker to compact; confirm it, or say why it could not. */
export async function compactChat(ops: Pick<ChatOps, 'compact'>, chatId: string, onError: (message: string) => void): Promise<void> {
  try {
    await ops.compact(chatId);
    toast.show({ kind: 'info', text: 'Context compacted.' });
  } catch (e) {
    onError(`Could not compact this chat: ${message(e)}`);
  }
}

/** "Roll over": the agent writes a hand-off, a new chat takes it, and this chat is archived. */
export async function rollOverChat(ops: Pick<ChatOps, 'rollOver'>, chatId: string, onError: (message: string) => void): Promise<void> {
  try {
    await ops.rollOver(chatId);
    toast.show({ kind: 'info', text: 'Rolling over: the agent is writing a hand-off. The new chat opens when it is ready.' });
  } catch (e) {
    onError(`Could not roll over this chat: ${message(e)}`);
  }
}

export function rollOverConfirm(title: string, onConfirm: () => void | Promise<unknown>): ConfirmSpec {
  return {
    title: 'Roll over this chat?',
    body: `The agent in “${title || 'Untitled chat'}” writes a short hand-off, a new chat in the same project starts from it, and this chat is archived. Running work in this chat is not carried over.`,
    confirm: 'Roll over',
    onConfirm,
  };
}
