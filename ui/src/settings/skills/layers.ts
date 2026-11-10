// The layer picker's logic, without React: which layers exist, which one a pane opens on, where an
// item can move to, and how a linked folder's reach reads. A layer is `everyone` or
// `profile:<profileId>` (the host's keys).

import type { SkillSource, SourceProjects } from './api.ts';

export const EVERYONE = 'everyone';

export interface LayerOption {
  /** The host's key: `everyone` or `profile:<id>`. */
  key: string;
  label: string;
  /** The suffix of the `knowledge-layer-<suffix>` test id: `everyone`, or the profile id. */
  testSuffix: string;
}

export interface LayerProfile {
  id: string;
  name: string;
  isHome: boolean;
}

export const profileLayer = (profileId: string): string => `profile:${profileId}`;

/** Everyone first, then the home profile, then the others in the order given. */
export function layerOptions(profiles: LayerProfile[]): LayerOption[] {
  const ordered = [...profiles.filter((p) => p.isHome), ...profiles.filter((p) => !p.isHome)];
  return [
    { key: EVERYONE, label: 'Everyone', testSuffix: 'everyone' },
    ...ordered.map((p) => ({ key: profileLayer(p.id), label: p.name, testSuffix: p.id })),
  ];
}

/** The layer a pane opens on: the active profile's, else the home profile's, else Everyone. */
export function defaultLayer(options: LayerOption[], activeId: string | null, homeId: string | null): string {
  for (const id of [activeId, homeId]) {
    if (id && options.some((o) => o.key === profileLayer(id))) return profileLayer(id);
  }
  return EVERYONE;
}

/** The layer to show: the picked one while it still exists, else the default. */
export function currentLayer(options: LayerOption[], picked: string | null, fallback: string): string {
  return picked && options.some((o) => o.key === picked) ? picked : fallback;
}

/** Every layer an item of `from` can move to. */
export function moveTargets(options: LayerOption[], from: string): LayerOption[] {
  return options.filter((o) => o.key !== from);
}

export function layerLabel(options: LayerOption[], key: string): string {
  return options.find((o) => o.key === key)?.label ?? (key === EVERYONE ? 'Everyone' : 'a removed profile');
}

/** Where a skill came from, in words: Helm's own folder or a linked one. */
export function sourceLabel(source: string, sources: SkillSource[]): string {
  if (!source.startsWith('linked:')) return 'In Helm';
  const found = sources.find((s) => s.id === source.slice('linked:'.length));
  const folder = found ? found.path.split(/[\\/]/).filter(Boolean).pop() : undefined;
  return folder ? `Linked: ${folder}` : 'Linked folder';
}

/** "All projects", or the chosen projects' names (an id with no project shows as is). */
export function projectsLabel(projects: SourceProjects, names: ReadonlyMap<string, string>): string {
  if (projects === 'all') return 'All projects';
  if (projects.length === 0) return 'No projects';
  return projects.map((id) => names.get(id) ?? id).join(', ');
}

/** The chosen projects with `id` switched on or off. */
export function toggleProject(ids: string[], id: string): string[] {
  return ids.includes(id) ? ids.filter((p) => p !== id) : [...ids, id];
}

/** How many skills a linked folder gave its layer, read from the layer's skill list. */
export function skillsFromSource(skills: { source: string }[], sourceId: string): number {
  return skills.filter((s) => s.source === `linked:${sourceId}`).length;
}
