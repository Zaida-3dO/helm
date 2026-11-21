// How the settings tabs are arranged: the tab bar is at most two rows — the first row names the
// groups, the second lists the panes of the chosen group. A pane keeps its id and its own `order`
// within its group; an id not listed here lands in "Other", last, so a pane added by a later build
// is never invisible.

export interface SettingsTabGroup<T> {
  title: string;
  panes: T[];
}

export interface SettingsTabGroupDef {
  title: string;
  ids: readonly string[];
}

export const OTHER_GROUP_TITLE = 'Other';

export const SETTINGS_TAB_GROUPS: readonly SettingsTabGroupDef[] = [
  {
    title: 'Chat & agents',
    ids: ['chat', 'runtime', 'localmodels', 'queue', 'skills', 'agents', 'memory', 'orchestrator', 'actions', 'schedules', 'afk', 'governor', 'gatekeeper'],
  },
  { title: 'Appearance', ids: ['general', 'appearance', 'shell', 'keyboard'] },
  { title: 'Tools & integrations', ids: ['projects', 'mcp', 'websearch', 'vaults', 'github', 'saved'] },
  { title: 'Machine & updates', ids: ['profiles', 'devices', 'hub', 'usage', 'data'] },
];

/**
 * Split panes (already in display order) into their groups. The group order is the order of
 * `SETTINGS_TAB_GROUPS`, with unknown ids gathered into "Other" at the end.
 */
export function groupSettingsTabs<T extends { id: string }>(panes: readonly T[]): SettingsTabGroup<T>[] {
  const groupOf = new Map<string, string>();
  for (const g of SETTINGS_TAB_GROUPS) for (const id of g.ids) groupOf.set(id, g.title);
  const groups: SettingsTabGroup<T>[] = [];
  for (const g of SETTINGS_TAB_GROUPS) {
    const members = panes.filter((p) => groupOf.get(p.id) === g.title);
    if (members.length > 0) groups.push({ title: g.title, panes: members });
  }
  const other = panes.filter((p) => !groupOf.has(p.id));
  if (other.length > 0) groups.push({ title: OTHER_GROUP_TITLE, panes: other });
  return groups;
}

export interface SettingsTabStrip<T> {
  /** The groups in row order (row 1 of the tab bar). */
  groups: SettingsTabGroup<T>[];
  /** The group whose panes row 2 shows. */
  activeGroup: SettingsTabGroup<T> | undefined;
  /** The pane shown in the body: the chosen one when it is in the active group, else that group's first. */
  current: T | undefined;
}

/**
 * The two-row tab bar state: `group` is the group last clicked (null until one is), `pane` the
 * pane last clicked (null until one is). A pick that no longer names a real pane falls back to
 * the group holding the shown pane, else the first group — so a registry change never leaves an
 * empty bar.
 */
export function settingsTabStrip<T extends { id: string }>(panes: readonly T[], group: string | null, pane: string | null): SettingsTabStrip<T> {
  const groups = groupSettingsTabs(panes);
  const shown = panes.find((p) => p.id === pane) ?? panes[0];
  const activeGroup =
    groups.find((g) => shown != null && g.title === group && g.panes.some((p) => p.id === shown.id)) ??
    groups.find((g) => shown != null && g.panes.some((p) => p.id === shown.id)) ??
    groups[0];
  const current = shown != null && activeGroup?.panes.some((p) => p.id === shown.id) ? shown : activeGroup?.panes[0];
  return { groups, activeGroup, current };
}

/**
 * Which edges of a scrolling pane have more content beyond them, so a soft fade can hint at it.
 * A pane that fits (or is within a pixel of its end) has none.
 */
export function scrollEdges(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
): { top: boolean; bottom: boolean } {
  return {
    top: scrollTop > 1,
    bottom: scrollTop + clientHeight < scrollHeight - 1,
  };
}
