// The unified model and effort picker's pure parts.

import type { LocalModelInfo, ModelCatalogue } from '../api/types.ts';
import { prettyModelName } from '../state/localModel.ts';

const EFFORT_LABELS: Record<string, string> = {
  off: 'No thinking',
  minimal: 'Minimal',
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  xhigh: 'Extra high',
  max: 'Max',
};

export const FALLBACK_EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];

export function effortLabel(effort: string): string {
  return EFFORT_LABELS[effort] ?? effort;
}

/**
 * A local model has no effort setting; the same stops set how much it thinks before answering.
 * Low turns thinking off, so it reads "No thinking"; the others read as the stop plus "thinking".
 */
export function thinkingLabel(effort: string): string {
  return effort === 'low' ? 'No thinking' : `${effortLabel(effort)} thinking`;
}

/** The ids of local models start with this, then the server's id, then the server's own model name. */
const LOCAL_PREFIX = 'local/';

export function isLocalModel(id: string | null | undefined): boolean {
  return !!id && id.startsWith(LOCAL_PREFIX);
}

/** The server id inside a local model id (`local/<server>/<model>`), or null. The model part may hold `/` and `:`. */
export function localServerId(id: string | null | undefined): string | null {
  if (!isLocalModel(id)) return null;
  const rest = id!.slice(LOCAL_PREFIX.length);
  const at = rest.indexOf('/');
  return at > 0 ? rest.slice(0, at) : null;
}

/**
 * The line beside the picker's "Check servers" button. Helm asks the local servers only when the
 * user does, so the list shows what they said last time; `checked` is whether this picker has
 * asked. A server that answered but has something to say (it lists no models, say) says it here.
 */
export function localHint(catalogue: ModelCatalogue | null, checked: boolean, notes: readonly string[] = []): string {
  const note = notes.find((n) => n.trim());
  if (note) return note;
  if ((catalogue?.local?.length ?? 0) > 0) return 'Local models';
  return checked ? 'Local models: none found' : 'Local models: not checked yet';
}

/** The model's own name inside a local model id (`local/<server>/<model>`). */
export function localModelName(id: string): string {
  const rest = id.slice(LOCAL_PREFIX.length);
  const at = rest.indexOf('/');
  return at >= 0 ? rest.slice(at + 1) : rest;
}

/** A context size as people write it: 262144 → "256k", 1000000 → "1M". */
export function formatContext(tokens: number | undefined): string {
  if (!tokens || tokens <= 0) return '';
  if (tokens >= 1_000_000) return `${Math.round(tokens / 100_000) / 10}M`;
  return tokens >= 1024 ? `${Math.round(tokens / 1024)}k` : String(tokens);
}

/** The model's name without a parenthesised suffix ("Opus 4.5 (1M)" → "Opus 4.5"). */
export function shortName(name: string): string {
  return name.replace(/\s*\([^)]*\)\s*$/, '').trim() || name;
}

/**
 * The trigger's label: "Model · Effort". A local model shows its name as the user chose it (an
 * alias is kept whole) and reads the lowest stop, which turns thinking off, as "No thinking".
 */
export function triggerLabel(modelName: string, effort: string, local = false): string {
  const { name, rest } = triggerParts(modelName, effort, local);
  return `${name} · ${rest}`;
}

/** The trigger's label in two parts: the name that always shows and the effort word that gives way first. */
export function triggerParts(modelName: string, effort: string, local = false): { name: string; rest: string } {
  if (local) return { name: modelName, rest: effort === 'low' ? 'No thinking' : effortLabel(effort) };
  return { name: shortName(modelName), rest: effortLabel(effort) };
}

/** One step left (-1) or right (+1) along the efforts, stopping at the ends. */
export function stepEffort(efforts: string[], current: string, dir: -1 | 1): string {
  if (efforts.length === 0) return current;
  const i = efforts.indexOf(current);
  if (i < 0) return efforts[dir > 0 ? 0 : efforts.length - 1]!;
  return efforts[Math.max(0, Math.min(efforts.length - 1, i + dir))]!;
}

export interface ModelRow {
  id: string;
  title: string;
  /** The name the trigger and the chip use: the user's name, or the default short name — never
   * the row title, which may carry both ("Jane · Qwen 3.8"). */
  name: string;
  detail: string;
  disabled: boolean;
  /** Set on every row when the list has local models too: "Claude" or "Local". */
  group?: 'Claude' | 'Local';
  /** Why the row is disabled, shown as its tooltip. */
  hint?: string;
}

/**
 * The user's own name for a model, when the row carries one. The app names an unaliased model by
 * its default short name (which a clash may lengthen with the model's next distinguishing word —
 * that still starts with the short name); any other name is one the user typed.
 */
function userAlias(name: string, model: string): string | null {
  const pretty = prettyModelName(model);
  if (!pretty || name === pretty || name.startsWith(`${pretty} `) || pretty === model) return null;
  return name;
}

function localRow(m: LocalModelInfo): ModelRow {
  // The row is titled by the name the app gives the model; its own name goes in the detail. An
  // alias rides with the default short name, so "Jane" reads "Jane · Qwen 3.8" and nobody has to
  // remember which pet name belongs to which model.
  const alias = userAlias(m.name, m.model);
  const title = alias ? `${alias} · ${prettyModelName(m.model)}` : m.name;
  const parts = [m.model !== m.name ? m.model : '', m.server, formatContext(m.contextTokens)].filter(Boolean);
  let hint: string | undefined;
  if (!m.reachable) {
    // A Strata Helm can start is choosable even while down: using it starts it.
    if (m.startable) parts.push('not running, starts when you use it');
    else {
      parts.push('not answering');
      hint = `${m.server} did not answer. Check that it is running, or look in Settings → Local models.`;
    }
  } else if (m.loaded === true) parts.push('loaded');
  return {
    id: m.id,
    title,
    name: m.name,
    detail: parts.join(' · '),
    disabled: !m.reachable && !m.startable,
    hint,
    group: 'Local',
  };
}

/** The local models as `{id, name, description}` choices, for a plain model list (Settings → Runtime's new-chat model). */
export function localModelChoices(local: readonly LocalModelInfo[] | undefined): { id: string; name: string; description: string }[] {
  return (local ?? []).map((m) => ({ id: m.id, name: m.name, description: localRow(m).detail }));
}

/**
 * The rows: the Claude models, then the local ones under their own heading, and (when the chat's
 * model is not among them) a disabled row for it, so the current choice is never silently
 * replaced. A chat can change its model at any time, in either direction: the conversation carries
 * over, so no row is greyed for having started. A local server that did not answer greys its rows
 * with the reason.
 */
export function modelRows(catalogue: ModelCatalogue | null, current: string | null): ModelRow[] {
  const claude: ModelRow[] = (catalogue?.models ?? []).map((m) => ({
    id: m.id,
    title: shortName(m.name),
    name: shortName(m.name),
    detail: m.description ? `Claude Code · ${m.description}` : 'Claude Code',
    disabled: false,
    group: 'Claude',
  }));
  const local = (catalogue?.local ?? []).map(localRow);
  if (current && ![...claude, ...local].some((r) => r.id === current)) {
    // A local model not listed (yet: the servers are only asked when the picker opens) reads as the
    // short name made from its id, never as the raw id.
    if (isLocalModel(current)) {
      const pretty = prettyModelName(localModelName(current));
      local.push({ id: current, title: pretty, name: pretty, detail: 'Not in the current model list', disabled: true, group: 'Local' });
    } else claude.push({ id: current, title: current, name: current, detail: 'Not in the current model list', disabled: true, group: 'Claude' });
  }
  const rows = [...claude, ...local];
  // Headings only help when there is more than one group.
  if (local.length === 0) for (const r of rows) delete r.group;
  return rows;
}

/**
 * "Claude Code's default" as the first row, under Claude when the list has headings. The new-chat
 * default offers it: choosing it stores no model, so Claude Code picks its own.
 */
export function withDefaultRow(rows: ModelRow[], id: string): ModelRow[] {
  const row: ModelRow = { id, title: "Claude Code's default", name: "Claude Code's default", detail: 'Claude Code picks the model', disabled: false };
  if (rows.some((r) => r.group)) row.group = 'Claude';
  return [row, ...rows];
}

/** Rows whose title, id or detail contains the query (case-insensitive). */
export function filterRows(rows: ModelRow[], q: string): ModelRow[] {
  const s = q.trim().toLowerCase();
  if (!s) return rows;
  return rows.filter((r) => `${r.title} ${r.id} ${r.detail}`.toLowerCase().includes(s));
}

/** What a commit changes: only the parts that differ from the chat's current choice. */
export function commitPatch(
  current: { model: string; effort: string },
  next: { model: string; effort: string },
): { model?: string; effort?: string } | null {
  const patch: { model?: string; effort?: string } = {};
  if (next.model && next.model !== current.model) patch.model = next.model;
  if (next.effort && next.effort !== current.effort) patch.effort = next.effort;
  return patch.model === undefined && patch.effort === undefined ? null : patch;
}
