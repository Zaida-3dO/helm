// Classifying a tool result row into a card kind, and pulling out its label and target.
// Tool names differ per harness; these are the Claude Code tool names, and anything unknown falls back to the generic card.

import type { MessageRow } from '../api/types.ts';
import { parseTodos, todoSummary } from './todos.ts';

export type ToolKind = 'shell' | 'read' | 'edit' | 'web' | 'mcp' | 'generic';

const SHELL = new Set(['Bash', 'PowerShell', 'BashOutput', 'KillShell', 'KillBash']);
const READ = new Set(['Read', 'Grep', 'Glob', 'LS']);
const EDIT = new Set(['Edit', 'MultiEdit', 'Write', 'NotebookEdit']);
const WEB = new Set(['WebSearch', 'WebFetch']);

export function toolKind(name: string | null): ToolKind {
  if (!name) return 'generic';
  if (SHELL.has(name)) return 'shell';
  if (READ.has(name)) return 'read';
  if (EDIT.has(name)) return 'edit';
  if (WEB.has(name)) return 'web';
  if (/^mcp__/.test(name)) return 'mcp';
  return 'generic';
}

/** "server · tool" for an MCP name `mcp__<server>__<tool>`, else the tool name. */
export function toolLabel(name: string | null): string {
  if (!name) return 'Tool';
  const m = /^mcp__(.+?)__(.+)$/.exec(name);
  return m ? `${m[1]} · ${m[2]}` : name;
}

function str(v: unknown): string | null {
  return typeof v === 'string' && v ? v : null;
}

/**
 * The call's arguments. The host sends them as JSON text cut to 4 KiB, so a cut string may not
 * parse; then the string fields that survived whole are pulled out one by one.
 */
export function toolArgs(row: MessageRow): Record<string, unknown> {
  const a: unknown = row.details?.toolArgs;
  if (a && typeof a === 'object') return a as Record<string, unknown>;
  if (typeof a !== 'string' || !a) return {};
  try {
    const v: unknown = JSON.parse(a);
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : {};
  } catch {
    const out: Record<string, unknown> = {};
    for (const m of a.matchAll(/"([A-Za-z_]+)"\s*:\s*("(?:[^"\\]|\\.)*")/g)) {
      try {
        out[m[1]!] = JSON.parse(m[2]!);
      } catch {
        // skip a field that does not decode
      }
    }
    return out;
  }
}

/** The one-line target: command, path, URL or query. */
export function toolTarget(row: MessageRow): string | null {
  const todos = parseTodos(row);
  if (todos) return todoSummary(todos);
  const a = toolArgs(row);
  return (
    str(a.command) ??
    str(a.file_path) ??
    str(a.notebook_path) ??
    str(a.path) ??
    str(a.pattern) ??
    str(a.url) ??
    str(a.query) ??
    str(a.description) ??
    null
  );
}

export type ToolStatus = 'running' | 'complete' | 'failed' | 'interrupted';

export function toolStatus(row: MessageRow): ToolStatus {
  if (row.status === 'streaming' || row.status === 'saved') return 'running';
  if (row.status === 'interrupted') return 'interrupted';
  if (row.status === 'error' || row.details?.isError) return 'failed';
  return 'complete';
}

/** Lines of a bounded change view for an edit or write: removed lines `-`, added `+`. */
export function changeLines(row: MessageRow, max = 400): { lines: { sign: ' ' | '-' | '+'; text: string }[]; cut: boolean } {
  const a = toolArgs(row);
  const out: { sign: ' ' | '-' | '+'; text: string }[] = [];
  const add = (sign: ' ' | '-' | '+', s: unknown) => {
    if (typeof s !== 'string') return;
    for (const text of s.split('\n')) out.push({ sign, text });
  };
  if (Array.isArray(a.edits)) {
    for (const e of a.edits as Record<string, unknown>[]) {
      add('-', e.old_string);
      add('+', e.new_string);
    }
  } else if (typeof a.old_string === 'string' || typeof a.new_string === 'string') {
    add('-', a.old_string);
    add('+', a.new_string);
  } else {
    add('+', a.content ?? a.new_source);
  }
  const cut = out.length > max || Boolean(row.details?.textTruncated);
  return { lines: out.slice(0, max), cut };
}

// ---------------------------------------------------------------------------------------------
// Tool identity and the flat row's wording.

export interface ToolIdentity {
  source: 'host' | 'mcp' | 'native';
  /** The MCP server name, or "Helm" for the app's own host tools. */
  server: string;
  /** The tool's own name, without the `mcp__<server>__` prefix. */
  tool: string;
}

/**
 * Where a tool came from. The worker sets `details.toolSource`; for rows without it (older rows,
 * or a worker that predates it) the `mcp__<server>__<tool>` name is read instead, and
 * `mcp__helm__` means a host tool.
 */
export function toolIdentity(row: MessageRow): ToolIdentity {
  const src = row.details?.toolSource;
  if (src && (src.kind === 'host' || src.kind === 'mcp' || src.kind === 'native')) {
    return { source: src.kind, server: src.server, tool: src.tool };
  }
  const m = /^mcp__(.+?)__(.+)$/.exec(row.toolName ?? '');
  if (!m) return { source: 'native', server: '', tool: row.toolName ?? '' };
  return m[1] === 'helm' ? { source: 'host', server: 'Helm', tool: m[2]! } : { source: 'mcp', server: m[1]!, tool: m[2]! };
}

/** "app_settings_get" or "list-files" to "Settings get" or "List files". */
export function readable(name: string): string {
  const s = name.replace(/^app_/, '').replace(/[_-]+/g, ' ').trim();
  return s ? s[0]!.toUpperCase() + s.slice(1) : name;
}

/** The kind icon a host tool carries, from the first word of its name. */
export function hostIconFor(tool: string): 'settings' | 'chat' | 'project' | 'bell' | 'bookmark' | 'clock' | 'cable' | 'reload' | 'wrench' {
  const w = tool.replace(/^app_/, '').split('_')[0] ?? '';
  switch (w) {
    case 'settings':
      return 'settings';
    case 'chat':
    case 'chats':
      return 'chat';
    case 'project':
    case 'projects':
      return 'project';
    case 'attention':
    case 'notify':
      return 'bell';
    case 'saved':
      return 'bookmark';
    case 'schedule':
    case 'schedules':
      return 'clock';
    case 'mcp':
      return 'cable';
    case 'reload':
    case 'restart':
      return 'reload';
    default:
      return 'wrench';
  }
}

// [running, done]
const VERBS: Record<string, [string, string]> = {
  Bash: ['Running', 'Ran'],
  PowerShell: ['Running', 'Ran'],
  BashOutput: ['Reading output of', 'Read output of'],
  KillShell: ['Stopping task', 'Stopped task'],
  KillBash: ['Stopping task', 'Stopped task'],
  TaskStop: ['Stopping task', 'Stopped task'],
  Read: ['Reading', 'Read'],
  Write: ['Writing', 'Wrote'],
  Edit: ['Editing', 'Edited'],
  MultiEdit: ['Editing', 'Edited'],
  NotebookEdit: ['Editing', 'Edited'],
  Grep: ['Searching', 'Searched'],
  Glob: ['Searching', 'Searched'],
  LS: ['Listing', 'Listed'],
  WebFetch: ['Fetching', 'Fetched'],
  WebSearch: ['Searching web', 'Searched web'],
  AskUserQuestion: ['Asking', 'Asked'],
  EnterPlanMode: ['Entering plan mode', 'Entered plan mode'],
  ExitPlanMode: ['Proposing plan', 'Proposed plan'],
  TodoWrite: ['Updating plan', 'Updated plan'],
  Task: ['Delegating', 'Delegated'],
  Agent: ['Delegating', 'Delegated'],
  Skill: ['Using skill', 'Used skill'],
  SendMessage: ['Messaging', 'Messaged'],
};

/** The verb that opens a collapsed row: live while running, past tense when done, "Failed" or "Stopped". */
export function toolVerb(row: MessageRow, status: ToolStatus = toolStatus(row)): string {
  if (status === 'failed') return 'Failed';
  if (status === 'interrupted') return 'Stopped';
  const id = toolIdentity(row);
  if (id.source !== 'native') return readable(id.tool);
  const v = row.toolName ? VERBS[row.toolName] : undefined;
  if (v) return status === 'running' ? v[0] : v[1];
  const name = readable(row.toolName ?? 'tool');
  return status === 'running' ? `Running ${name}` : name;
}

/** A path as filename and directory, for file tools. Both separators are read. */
export function splitPath(p: string): { name: string; dir: string } {
  const i = Math.max(p.lastIndexOf('/'), p.lastIndexOf(String.fromCharCode(92)));
  return i < 0 ? { name: p, dir: '' } : { name: p.slice(i + 1), dir: p.slice(0, i + 1) };
}

/** True for a tool whose target is a file path (shown as name then directory). */
export function isFileTool(kind: ToolKind): boolean {
  return kind === 'read' || kind === 'edit';
}
