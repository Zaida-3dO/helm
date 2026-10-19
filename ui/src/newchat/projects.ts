// Add project (Ctrl+O) and Primary folder…: the host calls, kept free of React
// so they are tested under node:test.

import type { Api } from '../api/client.ts';
import type { ProjectNode } from '../api/projects.ts';
import { folderName, samePath } from './places.ts';

/** A project over an existing folder; several projects may share one. */
export function addExistingFolder(api: Api, path: string, name?: string): Promise<ProjectNode> {
  const p = path.trim();
  return api<ProjectNode>('project_create', { name: (name ?? '').trim() || folderName(p), path: p });
}

/** A new folder by name, made by the host under `parent` (or its per-user projects folder). */
export function addNewFolder(api: Api, name: string, parent?: string | null): Promise<ProjectNode> {
  return api<ProjectNode>('project_create_folder', { name: name.trim(), ...(parent ? { parent } : {}) });
}

/** The folder a project's new chats start in today, if it has one. */
export function currentFolder(project: ProjectNode, projects: ProjectNode[]): string | null {
  if (project.path) return project.path;
  const primary = projects.find((p) => p.id === project.primaryFolderProjectId);
  return primary?.path ?? null;
}

/**
 * Primary folder…: a folder project (or a local group) gets the folder as its own path; a board
 * project points at a folder project over it, reusing an open one before making a new one. The
 * host refuses a change while any of the project's chats is working.
 */
export async function setPrimaryFolder(api: Api, project: ProjectNode, path: string, projects: ProjectNode[]): Promise<ProjectNode> {
  const folder = path.trim();
  if (project.kind === 'folder') {
    return api<ProjectNode>('project_update', { projectId: project.id, revision: project.revision, path: folder });
  }
  const open = projects.find((p) => p.kind === 'folder' && !p.archived && !!p.path && samePath(p.path, folder));
  const target = open ?? (await addExistingFolder(api, folder));
  return api<ProjectNode>('project_update', { projectId: project.id, revision: project.revision, primaryFolderProjectId: target.id });
}
