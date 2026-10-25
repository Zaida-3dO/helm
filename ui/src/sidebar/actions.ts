// The sidebar's actions, shared by the menus, drag and drop, hover buttons and shortcuts. Moves,
// nests, reorders and archives push an undo entry (undo.ts). Every action reports failures through
// `onError` as the host's own sentence; nothing here waits on the board.

import type { Api } from '../api/client.ts';
import type { ChatOps } from '../api/ops.ts';
import type { ChatOrganizeArgs, ProjectNode, ProjectOps, SidebarRow } from '../api/projects.ts';
import type { SidebarData } from './data.ts';
import { UndoStack } from './undo.ts';
import { toast } from '../ui/toasts.ts';

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

  reorderBefore(row: SidebarRow, beforeChatId: string, groupProjectId?: string): Promise<void> {
    const back = this.placement(row);
    return this.run(async () => {
      if (groupProjectId) await this.organize({ chatId: row.chatId, groupProjectId, parentChatId: null });
      await this.organize({ chatId: row.chatId, beforeChatId });
      if (groupProjectId) {
        this.d.undo.push({
          label: 'Move chat',
          undo: () => this.organize(back),
          redo: async () => {
            await this.organize({ chatId: row.chatId, groupProjectId, parentChatId: null });
            await this.organize({ chatId: row.chatId, beforeChatId });
          },
        });
      }
    }).then(() => undefined);
  }

  pin(row: SidebarRow, pinned: boolean): Promise<void> {
    return this.run(() => this.organize({ chatId: row.chatId, pinned })).then(() => undefined);
  }

  async archive(row: SidebarRow): Promise<void> {
    const ok = await this.run(() => this.d.ops.archive(row.chatId).then(() => true));
    if (!ok) return;
    await this.d.data.refetch([row.chatId]);
    this.d.undo.push({
      label: 'Archive chat',
      undo: async () => {
        await this.d.ops.restore(row.chatId);
        await this.d.data.refetch([row.chatId]);
      },
      redo: async () => {
        await this.d.ops.archive(row.chatId);
        await this.d.data.refetch([row.chatId]);
      },
    });
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

  rename(row: SidebarRow, title: string): Promise<unknown> {
    return this.run(() => this.d.api('chat_rename', { chatId: row.chatId, title }));
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

  reorderProject(p: ProjectNode, beforeProjectId: string): Promise<void> {
    return this.updateProject(p, { beforeProjectId });
  }

  async archiveProject(p: ProjectNode): Promise<void> {
    const ok = await this.run(() => this.d.pops.archive(p.id).then(() => true));
    if (!ok) return;
    void this.d.data.loadProjects();
    this.d.data.invalidatePaging();
    this.d.undo.push({
      label: 'Archive project',
      undo: async () => {
        await this.d.pops.restore(p.id);
        await this.d.data.loadProjects();
      },
      redo: async () => {
        await this.d.pops.archive(p.id);
        await this.d.data.loadProjects();
      },
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
