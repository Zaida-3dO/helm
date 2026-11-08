// Where a new chat will run and which board it is shown on: pure rules, so they are tested under
// node:test. A board project is where a chat is shown; a folder project is only where it runs (the
// folder its agent works in). Pickers list boards only.

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
 * that was open, the first open folder in sidebar order. A board with no folder yet skips the last
 * step: a chat in it should not land in an unrelated folder without being asked. (A "folder of the
 * project's most recently updated chat" step would need the sidebar's rows and is left out here.)
 */
export function resolveFolder(projectId: string | null, chosen: string | null, projects: ProjectNode[], openChatFolder: string | null): ResolvedFolder {
  if (chosen) return { folder: chosen, source: 'chosen' };
  const project = projects.find((p) => p.id === projectId);
  const own = primaryFolder(project, projects);
  if (own) return own;
  if (openChatFolder) return { folder: openChatFolder, source: 'open-chat' };
  if (project?.kind === 'board') return { folder: null, source: null };
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

/** A board a chat can be put on: on the board still, and not archived in Helm. */
export function isOpenBoard(p: ProjectNode | undefined): p is ProjectNode {
  return !!p && p.kind === 'board' && !p.archived && !p.closed && !p.board?.stale;
}

/** A board with no folder yet: new chats in it have nowhere to start until one is set. */
export function needsFolder(p: ProjectNode, projects: ProjectNode[]): boolean {
  return isOpenBoard(p) && primaryFolder(p, projects) === null;
}

/**
 * The projects every project picker lists (New chat in…, Move to project, the chat's Project tag):
 * boards only, since a folder is where a chat runs and not where it is shown. Archived boards and
 * boards no longer on the task board never show, nor do the `hidden` ones (the catch-all inbox),
 * and finished boards show only when `showCompleted` is on. The one already chosen (`currentId`)
 * always stays, whatever it is, so a picker never hides its own choice.
 */
export function pickerProjects(projects: ProjectNode[], showCompleted: boolean, currentId: string | null, hidden: readonly string[] = []): ProjectNode[] {
  const skip = new Set(hidden);
  return projects.filter((p) => p.id === currentId || (isOpenBoard(p) && !skip.has(p.id) && (showCompleted || !isFinished(p))));
}

/**
 * The board a project stands for: itself when it is an open board; for a folder project, the one
 * open, unfinished board whose folder it is (so a chat started from a folder lands where the host
 * would put it). Null when there is no such board, or more than one.
 */
export function boardFor(projectId: string | null | undefined, projects: ProjectNode[]): string | null {
  const p = projects.find((x) => x.id === projectId);
  if (!p) return null;
  if (p.kind === 'board') return isOpenBoard(p) ? p.id : null;
  const path = p.path;
  const over = projects.filter((b) => {
    if (!isOpenBoard(b) || isFinished(b)) return false;
    if (b.primaryFolderProjectId === p.id) return true;
    const folder = primaryFolder(b, projects)?.folder;
    return !!path && !!folder && samePath(folder, path);
  });
  return over.length === 1 ? over[0]!.id : null;
}

/**
 * Open boards that start their chats in the same folder, grouped by folder: only groups of two or
 * more are returned. Closed, archived, finished and off-board boards do not count. Such boards are
 * allowed, but a chat started from the folder cannot tell which one it belongs to, so settings
 * point them out.
 */
export function sharedFolders(projects: ProjectNode[]): { folder: string; boards: ProjectNode[] }[] {
  const groups: { folder: string; boards: ProjectNode[] }[] = [];
  for (const b of projects) {
    if (!isOpenBoard(b) || b.closed || isFinished(b)) continue;
    const folder = primaryFolder(b, projects)?.folder;
    if (!folder) continue;
    const g = groups.find((x) => samePath(x.folder, folder));
    if (g) g.boards.push(b);
    else groups.push({ folder, boards: [b] });
  }
  return groups.filter((g) => g.boards.length > 1);
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
