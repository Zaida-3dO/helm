// The Skills & rules screen's logic, without React: which layers exist, which one a pane opens on,
// where an item can move to, how a linked folder's reach reads, and how a rule's trigger dates read
// (badges, the filter chips and their sort). A layer is `everyone` or `profile:<profileId>` (the
// host's keys).

import type { RuleRow, RulesBudget, SkillSource, SourceProjects } from './api.ts';
import type { RuleWhen } from '../../api/generated/skills/RuleWhen.ts';

/** Today's local date as `YYYY-MM-DD`, the shape rule headers use. */
export function localToday(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

/** The date state a trigger's dates give as of `today` (both `YYYY-MM-DD`). */
export interface RuleDates {
  /** Past its last day: chats see nothing and Settings says Expired. */
  expired: boolean;
  /** Past its review day: it still applies, but asks to keep or delete. */
  reviewDue: boolean;
  /** Its start day has not come yet. */
  notYet: boolean;
}

export function ruleDates(when: Pick<RuleWhen, 'from' | 'until' | 'review'> | null | undefined, today: string): RuleDates {
  return {
    expired: !!when?.until && when.until < today,
    reviewDue: !!when?.review && when.review < today,
    notYet: !!when?.from && when.from > today,
  };
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** A `YYYY-MM-DD` date in short words ("31 Jan", "31 Jan 2027" when its year is not today's). */
export function dateLabel(iso: string | null | undefined, today: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso ?? '');
  if (!m || !MONTHS[Number(m[2]) - 1]) return iso ?? '';
  const day = String(Number(m[3]));
  const label = `${day} ${MONTHS[Number(m[2]) - 1]}`;
  return m[1] === today.slice(0, 4) ? label : `${label} ${m[1]}`;
}

/** True when any trigger key is set. Matches how the host decides a rule has a When. */
export function ruleHasTrigger(when: RuleWhen | null | undefined): boolean {
  if (!when) return false;
  const one = [when.show, when.otherwise, when.before, when.from, when.until, when.review].some((v) => v !== null && v !== undefined);
  const lists = [when.folders, when.hosts, when.pcs, when.tools, when.commands, when.kinds, when.people];
  return one || lists.some((l) => (l?.length ?? 0) > 0);
}

export interface RuleFlag {
  kind: 'expired' | 'upcoming' | 'review' | 'problem';
  label: string;
  title: string;
}

/** The badges a rule row shows. A rule is at most one of Expired, Not yet started and Review. */
export function ruleFlags(r: Pick<RuleRow, 'when'>, today: string): RuleFlag[] {
  const out: RuleFlag[] = [];
  const d = ruleDates(r.when, today);
  if (d.expired) out.push({ kind: 'expired', label: 'Expired', title: `ended on ${dateLabel(r.when?.until, today)}` });
  else if (d.notYet) out.push({ kind: 'upcoming', label: 'Not yet started', title: `starts on ${dateLabel(r.when?.from, today)}` });
  else if (d.reviewDue) out.push({ kind: 'review', label: 'Review: keep or delete?', title: `review was ${dateLabel(r.when?.review, today)}` });
  if ((r.when?.problems?.length ?? 0) > 0) out.push({ kind: 'problem', label: 'A header line could not be read', title: r.when?.problems.join('; ') ?? '' });
  return out;
}

/** The row filter chips. */
export const RULE_FILTERS = [
  { value: 'all', label: 'All' },
  { value: 'plain', label: 'Every chat' },
  { value: 'triggered', label: 'Triggered' },
  { value: 'expired', label: 'Expired' },
  { value: 'review', label: 'Review due' },
] as const;

export type RuleFilter = (typeof RULE_FILTERS)[number]['value'];

/** The list the Rules section draws: chip filter, text search, expired and review-due rows first. */
export function filterRules(rules: RuleRow[], filter: RuleFilter, query: string, today: string): RuleRow[] {
  const q = query.trim().toLowerCase();
  const kept = rules.filter((r) => {
    if (q && !`${r.title} ${r.path} ${r.whenSummary ?? ''}`.toLowerCase().includes(q)) return false;
    const d = ruleDates(r.when, today);
    switch (filter) {
      case 'plain': return !ruleHasTrigger(r.when);
      case 'triggered': return ruleHasTrigger(r.when);
      case 'expired': return d.expired;
      case 'review': return d.reviewDue || d.notYet;
      default: return true;
    }
  });
  const late = (r: RuleRow) => {
    const d = ruleDates(r.when, today);
    return d.expired || d.reviewDue ? 0 : 1;
  };
  return kept.sort((a, b) => late(a) - late(b));
}

export const EVERYONE = 'everyone';

/** The share of the whole-rule cap past which the Rules section warns about it. */
export const RULES_BUDGET_WARN_AT = 0.8;

/**
 * The sentence the Rules section shows when whole rules have used at least 80% of what every chat
 * gets, or null while there is room (or when the host reported no budget).
 */
export function rulesBudgetNote(budget: RulesBudget | null | undefined): string | null {
  if (!budget || budget.capBytes <= 0 || budget.usedBytes < budget.capBytes * RULES_BUDGET_WARN_AT) return null;
  const kb = (bytes: number) => Math.round(bytes / 1024);
  return `Rules kept whole use ${kb(budget.usedBytes)} KB of the ${kb(budget.capBytes)} KB chats get; past that the rest are only listed by name.`;
}

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

/**
 * The line under the picker: whose chats the list reaches, and, when it is another person's list,
 * a plain warning that it is not the viewer's own and that edits land in their profile. `own` is
 * the layer of the profile in use (null when there is none).
 */
export function layerNote(options: LayerOption[], layer: string, own: string | null): { text: string; other: boolean } {
  if (layer === EVERYONE) return { text: 'Loaded into every chat on this Helm.', other: false };
  const who = layerLabel(options, layer);
  if (layer === own || own === null) return { text: `Loaded into ${who}'s chats only, on any device.`, other: false };
  return {
    text: `This is ${who}'s list, not yours. It is loaded into ${who}'s chats only, on any device, and what you change here changes their profile.`,
    other: true,
  };
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
