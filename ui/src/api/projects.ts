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
    /** Archived and closed projects are left out unless asked for. */
    list: (includeArchived = false, includeClosed = false) => api<ProjectNode[]>('project_list', { includeArchived, includeClosed }),
    create: (a: ProjectCreateArgs) => api<ProjectNode>('project_create', a),
    update: (a: ProjectUpdateArgs) => api<ProjectNode>('project_update', a),
    archive: (projectId: string) => api<ProjectNode>('project_archive', { projectId }),
    restore: (projectId: string) => api<ProjectNode>('project_restore', { projectId }),
    close: (projectId: string) => api<ProjectNode>('project_close', { projectId }),
    reopen: (projectId: string) => api<ProjectNode>('project_reopen', { projectId }),
    page: (view: SidebarView, cursor?: string, groupId?: string) =>
      api<SidebarPage>('sidebar_page', { view, cursor, groupId }),
    rows: (chatIds: string[]) => api<SidebarRow[]>('sidebar_rows', { chatIds }),
    organize: (a: ChatOrganizeArgs) => api<SidebarRow[]>('chat_organize', a),
    connection: () => api<BoardConnection>('board_connection_get', {}),
    saveConnection: (url: string, token?: string) =>
      api<BoardConnection>('board_connection_save', token === undefined ? { url } : { url, token }),
    sync: () => api<BoardSyncState>('board_sync', {}),
    /** Set a project's picture: raw base64 of a PNG (at most 64 KB decoded). The project's icon becomes `img`. */
    iconImageSet: (projectId: string, pngBase64: string) => api<ProjectNode>('project_icon_image_set', { projectId, pngBase64 }),
    /** A project's picture as raw base64 of a PNG, or null when it has none. */
    iconImageGet: (projectId: string) => api<{ pngBase64: string | null }>('project_icon_image_get', { projectId }),
  };
}

export type ProjectOps = ReturnType<typeof projectOps>;
