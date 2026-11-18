// What the chip row by the composer shows, as plain functions: which modes are on, what the
// "+ mode" menu lists, how a change shows before the host has answered, and what a chat that has
// not started yet shows (its project's default modes, already on).

import type { ProjectNode } from '../api/projects.ts';
import type { ActiveMode, ChatModes, ChatModesChange, ModeBrief } from './api.ts';

const byName = (a: { name: string }, b: { name: string }) => a.name.localeCompare(b.name);

/** One entry of the "+ mode" menu; `off` marks a mode that was switched off in this chat. */
export interface MenuMode extends ModeBrief {
  off: boolean;
}

/**
 * The default modes a project's new chats start with. Like the startup set, a board project's own
 * list wins and an empty one falls back to the folder project it starts chats in.
 */
export function defaultsOf(project: ProjectNode | undefined, all: ProjectNode[]): string[] {
  if (!project) return [];
  if (project.defaultModes.length > 0) return project.defaultModes;
  const folder = project.primaryFolderProjectId ? all.find((p) => p.id === project.primaryFolderProjectId) : undefined;
  return folder?.defaultModes ?? [];
}

/**
 * The modes as the chips show them. Before a chat has started the host has not put anything on yet,
 * so the project's defaults that are still just available show as on (source `project`). Anything
 * the host already lists as on or off is left as it says.
 */
export function shown(modes: ChatModes, started: boolean, defaults: string[]): ChatModes {
  if (started || defaults.length === 0) return modes;
  const wanted = new Set(defaults.map((d) => d.toLowerCase()));
  const promoted: ActiveMode[] = modes.available.filter((m) => wanted.has(m.name.toLowerCase())).map((m) => ({ ...m, source: 'project' }));
  if (promoted.length === 0) return modes;
  const names = new Set(promoted.map((m) => m.name));
  return {
    available: modes.available.filter((m) => !names.has(m.name)),
    active: [...modes.active, ...promoted].sort(byName),
    off: modes.off,
  };
}

/** Whether the row has anything to show: a chat with no mode files in reach shows no row at all. */
export function hasModes(modes: ChatModes): boolean {
  return modes.available.length + modes.active.length + modes.off.length > 0;
}

/** The "+ mode" menu: every mode not on, in name order, the ones switched off in this chat marked. */
export function menuModes(modes: ChatModes): MenuMode[] {
  return [...modes.available.map((m) => ({ ...m, off: false })), ...modes.off.map((m) => ({ ...m, off: true }))].sort(byName);
}

/** What the lists look like once a change has been made: the answer to show until the host's own arrives. */
export function applyChange(modes: ChatModes, change: ChatModesChange): ChatModes {
  let { available, active, off } = modes;
  for (const name of change.add ?? []) {
    const m = [...available, ...off].find((x) => x.name === name);
    if (!m || active.some((a) => a.name === name)) continue;
    available = available.filter((x) => x.name !== name);
    off = off.filter((x) => x.name !== name);
    active = [...active, { name: m.name, description: m.description, layer: m.layer, source: 'user' }];
  }
  for (const name of change.remove ?? []) {
    const m = active.find((x) => x.name === name);
    if (!m) continue;
    active = active.filter((x) => x.name !== name);
    off = [...off, { name: m.name, description: m.description, layer: m.layer }];
  }
  return { available, active: [...active].sort(byName), off: [...off].sort(byName) };
}
