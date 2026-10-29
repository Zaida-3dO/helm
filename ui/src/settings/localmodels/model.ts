// Settings → Local models: the shapes the host answers with (generated bindings, re-exported here)
// and the pure helpers the pane renders with. The key of a server is write-only: `ServerView` only
// says whether one is set.

import type { CompareModel, LocalModelsState, Range, ServerView, ToolsSetup } from '../../api/generated/localmodels/index.ts';

export type { CompareModel, LocalModelsState, Range, ServerView, ToolsSetup };
export type { RefreshResult, TestResult } from '../../api/generated/localmodels/index.ts';

export const TOOLS_OPTIONS: { value: ToolsSetup; title: string; subtitle: string }[] = [
  { value: 'lean', title: 'Lean', subtitle: "Helm's own tools only; your Claude Code MCP servers stay off" },
  { value: 'full', title: 'Full', subtitle: 'The same tools and MCP servers as a Claude chat' },
];

export const COMPARE_OPTIONS: { value: CompareModel; title: string }[] = [
  { value: 'opus', title: 'Opus' },
  { value: 'sonnet', title: 'Sonnet' },
  { value: 'haiku', title: 'Haiku' },
];

/** One server as the pane edits it. `id` is empty for a server that has not been saved yet. */
export interface ServerDraft {
  /** Identifies the card while editing, including before the host has given the server an id. */
  key: string;
  id: string;
  name: string;
  kind: 'strata' | 'ollama';
  baseUrl: string;
  enabled: boolean;
  /** Strata only: the model to offer when the server does not list its models. */
  model: string;
}

export function toDraft(s: ServerView): ServerDraft {
  return { key: s.id, id: s.id, name: s.name, kind: s.kind, baseUrl: s.baseUrl, enabled: s.enabled, model: s.model ?? '' };
}

let fresh = 0;
/** A server the user is adding: no id until the host saves it, and no address (the host refuses anything but this machine). */
export function newDraft(kind: ServerDraft['kind']): ServerDraft {
  fresh += 1;
  return { key: `new-${fresh}`, id: '', name: kind === 'strata' ? 'Strata' : 'Ollama', kind, baseUrl: '', enabled: true, model: '' };
}

const FIELDS = ['id', 'name', 'kind', 'baseUrl', 'enabled', 'model'] as const;

/** Whether two lists would save the same servers (the cards' keys do not count). */
export function sameDrafts(a: readonly ServerDraft[], b: readonly ServerDraft[]): boolean {
  return a.length === b.length && a.every((x, i) => FIELDS.every((f) => x[f] === b[i]![f]));
}

/** What `local_models_save` takes for the server list; it replaces the whole list. */
export function savePayload(drafts: readonly ServerDraft[]): { id: string; name: string; kind: ServerDraft['kind']; baseUrl: string; enabled: boolean; model?: string }[] {
  return drafts.map((d) => ({
    id: d.id,
    name: d.name.trim(),
    kind: d.kind,
    baseUrl: d.baseUrl.trim(),
    enabled: d.enabled,
    ...(d.kind === 'strata' && d.model.trim() ? { model: d.model.trim() } : {}),
  }));
}

/**
 * A problem with a draft that can be seen without asking the host: no name, or an address that is
 * not plain http to this machine. The host refuses the same on save; this only saves a round trip.
 */
export function draftProblem(d: ServerDraft): string | null {
  if (!d.name.trim()) return 'Give the server a name.';
  const url = d.baseUrl.trim();
  if (!url) return 'Give the address, for example http://127.0.0.1:8080.';
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return 'That address is not a valid URL.';
  }
  const here = u.hostname === '127.0.0.1' || u.hostname === 'localhost' || u.hostname === '[::1]';
  if (u.protocol !== 'http:' || !here) return 'A local model server must be on this machine and use plain http (127.0.0.1, localhost or ::1).';
  return null;
}

/** What the card says about whether the server answered. */
export function reachText(s: ServerView | undefined): { text: string; tone: 'ok' | 'bad' | 'idle' } {
  if (!s || s.reachable === undefined) return { text: 'Not checked yet', tone: 'idle' };
  if (s.reachable) return { text: 'Answering', tone: 'ok' };
  return { text: s.note || 'Did not answer', tone: 'bad' };
}

/** A typed number kept inside its range; `null` when it is not a number at all. */
export function clampTo(range: Range, text: string): number | null {
  const n = Number(text.replace(/[,\s_]/g, ''));
  if (!text.trim() || !Number.isFinite(n)) return null;
  return Math.min(range.max, Math.max(range.min, Math.round(n)));
}

/** "Between 8,192 and 262,144 (default 65,536)". */
export function rangeText(range: Range, unit = ''): string {
  const f = (n: number) => `${n.toLocaleString('en-US')}${unit}`;
  return `Between ${f(range.min)} and ${f(range.max)} (default ${f(range.default)})`;
}
