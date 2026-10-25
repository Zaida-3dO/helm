// Where a new chat will run: pure rules, so
// they are tested under node:test. A project here is any sidebar group (a folder project, a local
// group or a board project); the folder is where the chat's agent works.

import type { ProjectNode } from '../api/projects.ts';
import { isFinished } from '../sidebar/model.ts';

/** Why the place line shows the folder it shows. */
export type FolderSource = 'chosen' | 'project' | 'primary' | 'open-chat' | 'first-open' | null;

export interface ResolvedFolder {
  folder: string | null;
  source: FolderSource;
}

/** Paths compare case-insensitively and with either slash, as Windows does. */
export function samePath(a: string, b: string): boolean {
  const norm = (p: string) => p.trim().replace(/[\\/]+/g, '/').replace(/\/$/, '').toLowerCase();
  return norm(a) === norm(b);
}

const usable = (p: ProjectNode | undefined): p is ProjectNode & { path: string } => !!p && !p.archived && !!p.path;

/** Folder projects with a folder, in the order given (the host's sidebar order). */
export function openFolders(projects: ProjectNode[]): (ProjectNode & { path: string })[] {
  return projects.filter((p) => p.kind === 'folder' && usable(p)) as (ProjectNode & { path: string })[];
}

/** A project's own folder: its path, else its primary folder project's path. */
export function primaryFolder(project: ProjectNode | undefined, projects: ProjectNode[]): { folder: string; source: 'project' | 'primary' } | null {
  if (!project) return null;
  if (usable(project)) return { folder: project.path, source: 'project' };
  const primary = project.primaryFolderProjectId ? projects.find((p) => p.id === project.primaryFolderProjectId) : undefined;
  return usable(primary) ? { folder: primary.path, source: 'primary' } : null;
}

/**
 * First match wins: the folder the user chose, the project's primary folder, the folder of the chat
 * that was open, the first open folder in sidebar order. (A "folder of the project's most
 * recently updated chat" step would need the sidebar's rows and is left out here.)
 */
export function resolveFolder(projectId: string | null, chosen: string | null, projects: ProjectNode[], openChatFolder: string | null): ResolvedFolder {
  if (chosen) return { folder: chosen, source: 'chosen' };
  const own = primaryFolder(projects.find((p) => p.id === projectId), projects);
  if (own) return own;
  if (openChatFolder) return { folder: openChatFolder, source: 'open-chat' };
  const first = openFolders(projects)[0];
  return first ? { folder: first.path, source: 'first-open' } : { folder: null, source: null };
}

/**
 * The folder project `chat_create` should put the chat in: the chosen project itself when it is a
 * folder project over exactly that folder (several projects may share one folder),
 * else none, and the host finds or makes the folder project for the path.
 */
export function folderProjectFor(projectId: string | null, folder: string, projects: ProjectNode[]): string | null {
  const p = projects.find((x) => x.id === projectId);
  return p && p.kind === 'folder' && usable(p) && samePath(p.path, folder) ? p.id : null;
}

/**
 * The projects the new-chat picker lists: open ones only, unless `showCompleted` is on. Archived
 * projects never show, and neither do finished board projects, except the one the draft is already
 * in (`currentId`), which always stays so the picker never hides its own choice.
 */
export function pickerProjects(projects: ProjectNode[], showCompleted: boolean, currentId: string | null): ProjectNode[] {
  return projects.filter((p) => p.id === currentId || (!p.archived && (showCompleted || !isFinished(p))));
}

/** The last segment of a path, for labels. */
export function folderName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

// ---- Recent folders (the folder menu's second section) ---------------------------------------

export const RECENTS_KEY = 'helm.newchat.recentFolders';
/** The old sidebar path box remembered one folder here; it seeds the list once. */
export const LEGACY_LAST_FOLDER_KEY = 'helm.lastFolder';
export const MAX_RECENTS = 8;

type KV = Pick<Storage, 'getItem' | 'setItem'>;

export function readRecents(store: KV): string[] {
  let list: unknown = null;
  try {
    list = JSON.parse(store.getItem(RECENTS_KEY) ?? 'null');
  } catch {
    list = null;
  }
  const out = Array.isArray(list) ? list.filter((x): x is string => typeof x === 'string' && x.trim().length > 0) : [];
  if (out.length === 0) {
    const legacy = store.getItem(LEGACY_LAST_FOLDER_KEY)?.trim();
    if (legacy) out.push(legacy);
  }
  return out.slice(0, MAX_RECENTS);
}

/** Put `folder` first, without duplicates (by path), keeping at most MAX_RECENTS. */
export function pushRecent(store: KV, folder: string): string[] {
  const next = [folder, ...readRecents(store).filter((f) => !samePath(f, folder))].slice(0, MAX_RECENTS);
  store.setItem(RECENTS_KEY, JSON.stringify(next));
  return next;
}

export interface FolderMenuModel {
  /** Open folders (folder projects), the project's own folder first. */
  open: { path: string; name: string }[];
  /** Recently used folders that are not already listed as open. */
  recent: string[];
}

export function folderMenu(projects: ProjectNode[], project: ProjectNode | undefined, recents: string[]): FolderMenuModel {
  const own = primaryFolder(project, projects)?.folder ?? null;
  const open: { path: string; name: string }[] = [];
  const add = (path: string, name: string) => {
    if (!open.some((o) => samePath(o.path, path))) open.push({ path, name });
  };
  if (own) add(own, project?.path ? project.name : folderName(own));
  for (const p of openFolders(projects)) add(p.path, p.name);
  const recent = recents.filter((r) => !open.some((o) => samePath(o.path, r)));
  return { open, recent };
}
