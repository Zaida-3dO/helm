// The work-timeline rail's model: the chat's prompts and work as events, and how they fit
// a rail of a given height. Pure, so the rail and the tests share it.

import type { MessageRow } from '../api/types.ts';
import { changeLines, toolKind, toolStatus, toolTarget } from './toolKinds.ts';

export type EventKind = 'prompt' | 'edit' | 'write' | 'test' | 'command' | 'failed' | 'running';

export interface TimelineEvent {
  rowId: number;
  kind: EventKind;
  /** "Edited file", "Tests passed", "Command failed"… */
  title: string;
  /** The path, command or prompt, one line. */
  detail: string;
  /** For the preview card: a prompt's full text (bounded), else ''. */
  body: string;
  /** For the preview card: the first lines of an edit's or write's recorded change. */
  diff: { sign: ' ' | '-' | '+'; text: string }[];
  /** For the preview card: a tool's output (bounded), else ''. */
  output: string;
}

export const PREVIEW_BODY_MAX = 800;
export const PREVIEW_DIFF_LINES = 10;
export const PREVIEW_OUTPUT_MAX = 600;

const TEST_COMMAND = /\b(test|tests|vitest|jest|pytest|mocha|playwright|cargo\s+test|go\s+test|npm\s+t)\b/i;

/** Which way a tool row reads on the rail, or null for tools the rail does not show (reads, searches). */
function toolEvent(row: MessageRow): Pick<TimelineEvent, 'kind' | 'title' | 'detail'> | null {
  const kind = toolKind(row.toolName);
  const status = toolStatus(row);
  const target = (toolTarget(row) ?? '').replace(/\s+/g, ' ').trim();
  if (kind !== 'edit' && kind !== 'shell') return null;
  const write = row.toolName === 'Write';
  if (status === 'failed') return { kind: 'failed', title: kind === 'shell' ? 'Command failed' : write ? 'Write failed' : 'Edit failed', detail: target };
  if (status === 'running' || status === 'interrupted') {
    const verb = status === 'running' ? 'Running' : 'Stopped';
    return { kind: 'running', title: kind === 'shell' ? `${verb} command` : `${verb} ${write ? 'write' : 'edit'}`, detail: target };
  }
  if (kind === 'edit') return write ? { kind: 'write', title: 'Wrote file', detail: target } : { kind: 'edit', title: 'Edited file', detail: target };
  if (TEST_COMMAND.test(target)) return { kind: 'test', title: 'Tests passed', detail: target };
  return { kind: 'command', title: 'Ran command', detail: target };
}

/** Prompts and work, oldest first. A steer read mid-turn counts as a prompt. */
export function timelineEvents(rows: readonly MessageRow[]): TimelineEvent[] {
  const out: TimelineEvent[] = [];
  for (const row of rows) {
    if (row.role === 'user') {
      out.push({ rowId: row.id, kind: 'prompt', title: 'Prompt', detail: row.text.replace(/\s+/g, ' ').trim().slice(0, 160), body: row.text.trim().slice(0, PREVIEW_BODY_MAX), diff: [], output: '' });
    } else if (row.role === 'toolResult') {
      const e = toolEvent(row);
      if (!e) continue;
      const isEdit = toolKind(row.toolName) === 'edit';
      out.push({ rowId: row.id, ...e, body: '', diff: isEdit ? changeLines(row, PREVIEW_DIFF_LINES).lines : [], output: isEdit && e.kind !== 'failed' ? '' : row.text.trim().slice(0, PREVIEW_OUTPUT_MAX) });
    }
  }
  return out;
}

/** The rail shows only once the chat has done some work, not for prompts alone. */
export function hasWork(events: readonly TimelineEvent[]): boolean {
  return events.some((e) => e.kind !== 'prompt');
}

/** Which markers have a row currently on screen: the first and last, or null when none does. */
export function visibleSpan(markers: readonly RailMarker[], minRowId: number, maxRowId: number): { first: number; last: number } | null {
  let first = -1;
  let last = -1;
  markers.forEach((m, i) => {
    if (m.event.rowId < minRowId || m.event.rowId > maxRowId) return;
    if (first < 0) first = i;
    last = i;
  });
  return first < 0 ? null : { first, last };
}

const PRIORITY: Record<EventKind, number> = { failed: 6, running: 5, edit: 4, write: 4, test: 3, command: 2, prompt: 1 };

export interface RailMarker {
  /** The event the marker stands for: the most telling one in its cluster. */
  event: TimelineEvent;
  /** How many events share the marker (more than 1 draws the cluster bar). */
  size: number;
}

/** One slot per 22 px, 8 to 64 slots; when there are more events than slots, neighbours cluster. */
export function railMarkers(events: readonly TimelineEvent[], height: number): RailMarker[] {
  const slots = Math.max(8, Math.min(64, Math.floor(height / 22)));
  if (events.length <= slots) return events.map((event) => ({ event, size: 1 }));
  const out: RailMarker[] = [];
  for (let i = 0; i < slots; i++) {
    const group = events.slice(Math.floor((i * events.length) / slots), Math.floor(((i + 1) * events.length) / slots));
    if (!group.length) continue;
    const event = group.reduce((best, e) => (PRIORITY[e.kind] > PRIORITY[best.kind] ? e : best));
    out.push({ event, size: group.length });
  }
  return out;
}
