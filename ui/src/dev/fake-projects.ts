// The dev fake host's project and sidebar ops, for checking
// the sidebar in a plain browser. fakeHost.ts registers it through `registerFakeOps`; chats come
// from the fake host's own `chat_list`. It seeds a three-level tree of board and folder projects.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { BoardSyncState, ProjectNode, SidebarRow } from '../api/projects.ts';
import type { ChatRecord } from '../api/types.ts';
import type { FakeOpContext, FakeOpHandler } from './fakeHost.ts';

type Emit = (payload: unknown) => void;

export function projectsFakeOps(opts: { seedBoard?: boolean } = { seedBoard: true }): { prefixes: string[]; handler: FakeOpHandler } {
  const projects = new Map<string, ProjectNode>();
  const chats = new Map<string, { chat: ChatRecord; group: string | null; parent: string | null; pinned: boolean; archived: boolean; position: number | null }>();
  let rev = 1;
  let url = 'https://standup.example';
  let hasToken = false;
  let sync: BoardSyncState = { configured: false, syncing: false, lastSyncAt: null, error: null, projectCount: 0 };

  const node = (id: string, name: string, over: Partial<ProjectNode> = {}): ProjectNode => ({
    id,
    kind: 'folder',
    name,
    path: null,
    parentId: null,
    parentSource: null,
    icon: null,
    colour: null,
    pinned: false,
    collapsed: false,
    position: null,
    archived: false,
    board: null,
    primaryFolderProjectId: null,
    chatCount: 0,
    createdAt: Date.now(),
    revision: 0,
    ...over,
  });
  const board = (id: string, title: string, parentId: string | null = null): ProjectNode =>
    node(id, title, {
      kind: 'board',
      parentId,
      parentSource: parentId ? 'local' : null,
      board: { boardProjectId: id, url, title, headline: null, area: 'desktop-app', repo: 'helm', priority: 'p2', lastActivity: new Date().toISOString(), stale: false, editedLocally: false },
    });
  if (opts.seedBoard) {
    for (const p of [board('b-helm', 'Helm'), board('b-site', 'Client Website'), node('f-w3', 'Release 3', { parentId: 'b-helm', parentSource: 'local', colour: 'teal' }), node('f-side', 'Sidebar', { parentId: 'f-w3', parentSource: 'local' })]) {
      projects.set(p.id, p);
    }
  }

  const groupOf = (id: string) => {
    const c = chats.get(id)!;
    return c.group ?? c.chat.projectId;
  };
  const toRow = (id: string): SidebarRow => {
    const c = chats.get(id)!;
    const busy = c.chat.status === 'starting' || c.chat.status === 'running' || c.chat.status === 'stopping';
    return {
      chatId: id,
      title: c.chat.title,
      groupProjectId: groupOf(id),
      folderProjectId: c.chat.projectId,
      folderPath: c.chat.cwd,
      depth: 0,
      parentChatId: c.parent,
      hasChildren: [...chats.values()].some((k) => k.parent === id),
      status: c.chat.status,
      attention: busy ? 'running' : c.chat.status === 'interrupted' ? 'interrupted' : c.pinned ? 'pinned' : null,
      pinned: c.pinned,
      unread: c.chat.unread.markToken !== null || c.chat.unread.completionId !== null,
      muted: c.chat.muted,
      stalled: c.chat.stalled,
      lastActivityAt: c.chat.lastActivityAt,
      archived: c.archived,
      position: c.position,
      createdAt: c.chat.createdAt,
      updatedAt: c.chat.updatedAt,
      revision: rev++,
    };
  };
  const list = (): ProjectNode[] =>
    [...projects.values()]
      .map((p) => ({ ...p, chatCount: [...chats.keys()].filter((id) => groupOf(id) === p.id && !chats.get(id)!.archived).length }))
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || (a.position ?? 1e9) - (b.position ?? 1e9) || b.createdAt - a.createdAt);
  const observe = (c: ChatRecord) => {
    const prev = chats.get(c.id);
    chats.set(c.id, { chat: c, group: prev?.group ?? null, parent: prev?.parent ?? null, pinned: prev?.pinned ?? false, archived: prev?.archived ?? false, position: prev?.position ?? null });
    if (!projects.has(c.projectId)) {
      projects.set(c.projectId, node(c.projectId, c.cwd.split(/[\\/]/).filter(Boolean).pop() ?? c.cwd, { path: c.cwd }));
    }
  };

  const handler: FakeOpHandler = (op, args, ctx: FakeOpContext) => {
    const emit: Emit = ctx.emit;
    for (const c of (ctx.call('chat_list', {}) as ChatRecord[] | undefined) ?? []) observe(c);
    switch (op) {
      case 'project_list':
        return list().filter((p) => args.includeArchived || !p.archived);
      case 'project_create': {
        const p = node(`p-${rev++}`, String(args.name), { path: (args.path as string) ?? null, parentId: (args.parentId as string) ?? null });
        projects.set(p.id, p);
        emit({ kind: 'project', project: p });
        return p;
      }
      case 'project_update': {
        const p = projects.get(String(args.projectId));
        if (!p) throw new ApiError('not_found', 'That project does not exist.');
        const next: ProjectNode = { ...p, revision: p.revision + 1 };
        if (typeof args.name === 'string') next.name = args.name;
        if ('icon' in args) next.icon = (args.icon as string) ?? null;
        if ('colour' in args) next.colour = (args.colour as string) ?? null;
        if (typeof args.pinned === 'boolean') next.pinned = args.pinned;
        if (typeof args.collapsed === 'boolean') next.collapsed = args.collapsed;
        if ('parentId' in args) next.parentId = (args.parentId as string) ?? null;
        if (next.board && typeof args.name === 'string') next.board = { ...next.board, editedLocally: args.name !== next.board.title };
        projects.set(p.id, next);
        emit({ kind: 'project', project: next });
        return next;
      }
      case 'project_archive':
      case 'project_restore': {
        const p = projects.get(String(args.projectId))!;
        const archived = op === 'project_archive';
        const ids = [...chats.keys()].filter((id) => groupOf(id) === p.id);
        ids.forEach((id) => (chats.get(id)!.archived = archived));
        const next = { ...p, archived, revision: p.revision + 1 };
        projects.set(p.id, next);
        emit({ kind: 'project', project: next });
        emit({ kind: 'sidebar', chatIds: ids });
        return next;
      }
      case 'sidebar_page': {
        const view = String(args.view);
        const ids = [...chats.keys()].filter((id) => (view === 'archived' ? chats.get(id)!.archived : !chats.get(id)!.archived));
        const rows = ids.map(toRow).sort((a, b) => (view === 'activity' ? b.updatedAt - a.updatedAt : b.createdAt - a.createdAt));
        const at = Number(args.cursor ?? 0);
        return { rows: rows.slice(at, at + 500), nextCursor: at + 500 < rows.length ? String(at + 500) : null };
      }
      case 'sidebar_rows':
        return (args.chatIds as string[]).filter((id) => chats.has(id)).map(toRow);
      case 'chat_organize': {
        const id = String(args.chatId);
        const c = chats.get(id)!;
        if (typeof args.groupProjectId === 'string') c.group = args.groupProjectId === c.chat.projectId ? null : args.groupProjectId;
        if ('parentChatId' in args) c.parent = (args.parentChatId as string) ?? null;
        if (typeof args.pinned === 'boolean') c.pinned = args.pinned;
        emit({ kind: 'sidebar', chatIds: [id] });
        return [toRow(id)];
      }
      case 'board_connection_get':
        return { url, hasToken };
      case 'board_connection_save':
        url = String(args.url);
        if (typeof args.token === 'string') hasToken = args.token.length > 0;
        return { url, hasToken };
      case 'board_sync':
        sync = { configured: hasToken, syncing: false, lastSyncAt: Date.now(), error: hasToken ? null : 'Set a token first.', projectCount: [...projects.values()].filter((p) => p.kind === 'board').length };
        emit({ kind: 'board_sync', state: sync });
        return sync;
    }
    throw new ApiError('refused', `fake-projects has no op ${op}`);
  };
  return { prefixes: ['project_', 'sidebar_', 'chat_organize', 'board_'], handler };
}
