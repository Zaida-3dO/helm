// The unified model and effort picker's pure parts.

import type { ModelCatalogue } from '../api/types.ts';

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

/** The model's name without a parenthesised suffix ("Opus 4.5 (1M)" → "Opus 4.5"). */
export function shortName(name: string): string {
  return name.replace(/\s*\([^)]*\)\s*$/, '').trim() || name;
}

/** The trigger's label: "Model · Effort". */
export function triggerLabel(modelName: string, effort: string): string {
  return `${shortName(modelName)} · ${effortLabel(effort)}`;
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
  detail: string;
  disabled: boolean;
}

/**
 * The rows: the catalogue's models, then (when the chat's model is not among them) a disabled
 * row for it, so the current choice is never silently replaced.
 */
export function modelRows(catalogue: ModelCatalogue | null, current: string | null): ModelRow[] {
  const rows: ModelRow[] = (catalogue?.models ?? []).map((m) => ({
    id: m.id,
    title: shortName(m.name),
    detail: m.description ? `Claude Code · ${m.description}` : 'Claude Code',
    disabled: false,
  }));
  if (current && !rows.some((r) => r.id === current)) {
    rows.push({ id: current, title: current, detail: 'Not in the current model list', disabled: true });
  }
  return rows;
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
