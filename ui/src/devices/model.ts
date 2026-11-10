// Devices as plain data: which computer the sidebar shows, how a computer reads in the menu, and
// how another computer's chat records become the rows and projects the sidebar draws. No React,
// so the rules are tested under node:test.

import type { ProjectNode, SidebarRow } from '../api/projects.ts';
import { isLocalModelId } from '../state/localModel.ts';
import { rowAttention } from '../sidebar/status.ts';
import { lastSeenMs } from '../profiles/model.ts';
import type { DeviceInfo, DevicesList, RemoteChatRecord, RemoteProject } from './api.ts';

/**
 * The computer whose chats show: the stored choice when that computer is still on the hub, else
 * this PC. `null` while the list has not been read (or cannot be): the sidebar then shows this PC.
 */
export function resolveSelected(stored: string | null, list: DevicesList | null): string | null {
  if (!list) return null;
  return stored !== null && list.devices.some((d) => d.uid === stored) ? stored : list.self;
}

/** The selected computer when it is not this one. */
export function remoteDevice(list: DevicesList | null, selected: string | null): DeviceInfo | null {
  if (!list || selected === null || selected === list.self) return null;
  return list.devices.find((d) => d.uid === selected) ?? null;
}

/** The toggle only appears when there is another computer to switch to. */
export function showToggle(list: DevicesList | null): boolean {
  return !!list && list.devices.some((d) => d.uid !== list.self);
}

/** This PC first, then the others A to Z. */
export function menuOrder(list: DevicesList): DeviceInfo[] {
  const mine = list.devices.filter((d) => d.uid === list.self);
  const others = list.devices.filter((d) => d.uid !== list.self).sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));
  return [...mine, ...others];
}

/** "offline since 14:02" for a computer that is off, "online" otherwise. */
export function stateText(d: Pick<DeviceInfo, 'online' | 'lastSeenAt' | 'isSelf'>, now: number): string {
  if (d.online || d.isSelf) return 'online';
  const seen = lastSeenMs(d.lastSeenAt);
  if (seen === null) return 'offline';
  const sameDay = new Date(seen).toDateString() === new Date(now).toDateString();
  const when = new Date(seen);
  return `offline since ${sameDay ? when.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) : when.toLocaleDateString()}`;
}

/** The menu line for a computer: "Name · this PC" or its name. */
export function menuLabel(d: DeviceInfo): string {
  return d.isSelf ? `${d.name} · this PC` : d.name;
}

/** The note under the Projects / Activity switch while another computer's chats show. */
export function scopeNote(d: DeviceInfo, now: number): string {
  return `Chats on ${d.name} · ${stateText(d, now)}`;
}

/** A project node for the sidebar from another computer's project list. */
export function remoteProject(p: RemoteProject, chatCount: number, createdAt = 0): ProjectNode {
  return {
    id: p.id,
    kind: p.kind === 'board' ? 'board' : 'folder',
    name: p.name,
    path: null,
    parentId: null,
    parentSource: null,
    icon: null,
    colour: p.colour,
    pinned: false,
    collapsed: false,
    position: null,
    archived: false,
    closed: false,
    board: null,
    primaryFolderProjectId: null,
    chatCount,
    createdAt,
    revision: 0,
  };
}

/** One of another computer's chats as a sidebar row. */
export function remoteRow(c: RemoteChatRecord, hasChildren: boolean): SidebarRow {
  const unread = c.unread.markToken !== null || c.unread.completionId !== null;
  return {
    chatId: c.id,
    title: c.title,
    groupProjectId: c.projectId,
    folderProjectId: c.projectId,
    folderPath: c.cwd,
    depth: 0,
    parentChatId: c.parentChatId,
    hasChildren,
    status: c.status,
    attention: rowAttention({ status: c.status, unread, lingering: false, pinned: c.pinned }),
    pinned: c.pinned,
    unread,
    muted: c.muted,
    stalled: c.stalled,
    local: isLocalModelId(c.model),
    localAhead: c.localAhead,
    hasDraft: c.draft.trim() !== '',
    lastActivityAt: c.lastActivityAt,
    archived: c.archived,
    position: null,
    createdAt: c.createdAt,
    updatedAt: c.updatedAt,
    revision: c.revision,
  };
}

/**
 * The rows and projects for another computer's chats. A chat whose project the computer did not
 * list gets one made from the name the chat carries, so no chat is left without a group.
 */
export function remoteList(chats: readonly RemoteChatRecord[], projects: readonly RemoteProject[]): { rows: SidebarRow[]; projects: ProjectNode[] } {
  const parents = new Set(chats.map((c) => c.parentChatId).filter((p): p is string => !!p));
  const rows = chats.map((c) => remoteRow(c, parents.has(c.id)));
  const known = new Map(projects.map((p) => [p.id, p]));
  for (const c of chats) {
    if (!known.has(c.projectId)) known.set(c.projectId, { id: c.projectId, name: c.projectName ?? 'Chats', colour: c.projectColour ?? null, kind: 'folder' });
  }
  const counts = new Map<string, number>();
  for (const r of rows) if (!r.archived) counts.set(r.groupProjectId, (counts.get(r.groupProjectId) ?? 0) + 1);
  const nodes = [...known.values()].map((p) => remoteProject(p, counts.get(p.id) ?? 0));
  return { rows, projects: nodes };
}
