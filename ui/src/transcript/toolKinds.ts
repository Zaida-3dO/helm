// Classifying a tool result row into a card kind, and pulling out its label and target.
// Tool names differ per harness; these are the Claude Code tool names, and anything unknown falls back to the generic card.

import type { MessageRow } from '../api/types.ts';

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
