// The unified model and effort picker's pure parts.

import type { LocalModelInfo, ModelCatalogue } from '../api/types.ts';

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
 * user does, so the list shows what they said last time; `checked` is whether this picker has asked.
 */
export function localHint(catalogue: ModelCatalogue | null, checked: boolean): string {
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

/** The trigger's label: "Model · Effort", or "Model · High thinking" for a local model. */
export function triggerLabel(modelName: string, effort: string, local = false): string {
  return `${shortName(modelName)} · ${local ? thinkingLabel(effort) : effortLabel(effort)}`;
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
  /** Set on every row when the list has local models too: "Claude" or "Local". */
  group?: 'Claude' | 'Local';
  /** Why the row is disabled, shown as its tooltip. */
  hint?: string;
}

function localRow(m: LocalModelInfo): ModelRow {
  const parts = [m.server, formatContext(m.contextTokens)].filter(Boolean);
  if (m.loaded === true && m.reachable) parts.push('loaded');
  return {
    id: m.id,
    title: m.name,
    detail: m.reachable ? parts.join(' · ') : `${m.server} · not answering`,
    disabled: !m.reachable,
    hint: m.reachable ? undefined : `${m.server} did not answer. Check that it is running, or look in Settings → Local models.`,
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
 * replaced. Once the chat has started (`fixed`) the model cannot cross between Claude and local,
 * or between local servers: those rows are disabled with a tooltip saying so. A local server that
 * did not answer greys its rows with the reason.
 */
export function modelRows(catalogue: ModelCatalogue | null, current: string | null, fixed = false): ModelRow[] {
  const claude: ModelRow[] = (catalogue?.models ?? []).map((m) => ({
    id: m.id,
    title: shortName(m.name),
    detail: m.description ? `Claude Code · ${m.description}` : 'Claude Code',
    disabled: false,
    group: 'Claude',
  }));
  const local = (catalogue?.local ?? []).map(localRow);
  if (current && ![...claude, ...local].some((r) => r.id === current)) {
    const orphan: ModelRow = { id: current, title: current, detail: 'Not in the current model list', disabled: true };
    // A local model not listed (yet: the servers are only asked when the picker opens) reads as its own name.
    if (isLocalModel(current)) local.push({ ...orphan, title: localModelName(current), group: 'Local' });
    else claude.push({ ...orphan, group: 'Claude' });
  }
  if (fixed) {
    const onLocal = isLocalModel(current);
    const server = localServerId(current);
    const startedOn = onLocal ? ((catalogue?.local ?? []).find((m) => m.id === current)?.server ?? 'a local model server') : 'Claude';
    for (const r of [...claude, ...local]) {
      const crosses = isLocalModel(r.id) !== onLocal || (onLocal && localServerId(r.id) !== server);
      if (!crosses) continue;
      r.disabled = true;
      r.hint = `The model is fixed for this chat: it started on ${startedOn}. Start a new chat to use this one.`;
    }
  }
  const rows = [...claude, ...local];
  // Headings only help when there is more than one group.
  if (local.length === 0) for (const r of rows) delete r.group;
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
