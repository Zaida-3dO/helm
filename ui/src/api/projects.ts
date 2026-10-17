// Typed wrappers over the project and sidebar ops. Every call still goes through
// api(op, args). The record shapes come from the generated bindings.

import type { Api } from './client.ts';
import type {
  BoardConnection,
  BoardSyncState,
  ChatOrganizeArgs,
  ProjectCreateArgs,
  ProjectNode,
  ProjectUpdateArgs,
  SidebarPage,
  SidebarRow,
  SidebarView,
} from './generated/projects/index.ts';

export type {
  BoardConnection,
  BoardInfo,
  BoardSyncState,
  ChatOrganizeArgs,
  ParentSource,
  ProjectKind,
  ProjectNode,
  ProjectUpdateArgs,
  RowAttention,
  SidebarPage,
  SidebarRow,
  SidebarView,
} from './generated/projects/index.ts';

export function projectOps(api: Api) {
  return {
    list: (includeArchived = false) => api<ProjectNode[]>('project_list', { includeArchived }),
    create: (a: ProjectCreateArgs) => api<ProjectNode>('project_create', a),
    update: (a: ProjectUpdateArgs) => api<ProjectNode>('project_update', a),
    archive: (projectId: string) => api<ProjectNode>('project_archive', { projectId }),
    restore: (projectId: string) => api<ProjectNode>('project_restore', { projectId }),
    page: (view: SidebarView, cursor?: string, groupId?: string) =>
      api<SidebarPage>('sidebar_page', { view, cursor, groupId }),
    rows: (chatIds: string[]) => api<SidebarRow[]>('sidebar_rows', { chatIds }),
    organize: (a: ChatOrganizeArgs) => api<SidebarRow[]>('chat_organize', a),
    connection: () => api<BoardConnection>('board_connection_get', {}),
    saveConnection: (url: string, token?: string) =>
      api<BoardConnection>('board_connection_save', token === undefined ? { url } : { url, token }),
    sync: () => api<BoardSyncState>('board_sync', {}),
  };
}

export type ProjectOps = ReturnType<typeof projectOps>;
